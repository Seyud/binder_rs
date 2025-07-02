extern crate bindgen;

use anyhow::Result;
use bindgen::EnumVariation;
use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

const CARGO_CONTENT: &str = r#"
[package]
name = "binder_ndk"
authors = ["Android"]
version = "1.0.0"
edition = "2021"
rust-version = "1.67"

[lib]
crate-type = ["cdylib"]

[workspace]
"#;

fn build_stub() -> Result<()> {
    let symbols = std::fs::read_to_string("src/symbols.txt")?;
    let outdir = env::var("OUT_DIR")?;
    let project_path = PathBuf::from(&outdir).join("libbinder_ndk");
    if project_path.exists() {
        std::fs::remove_dir_all(&project_path)?;
    }
    std::fs::create_dir(&project_path)?;

    let project_cargo_path = project_path.join("Cargo.toml");
    std::fs::File::create(&project_cargo_path)?;
    std::fs::write(&project_cargo_path, CARGO_CONTENT)?;
    let src_path = project_path.join("src");
    std::fs::create_dir_all(&src_path)?;
    let mut f = std::fs::File::create(src_path.join("lib.rs"))?;
    for symbol in symbols.split("\n") {
        if !symbol.is_empty() {
            f.write_all(format!("#[no_mangle]\npub extern \"C\" fn {}() {{}}\n", symbol).as_bytes())?;
        }
    }
    f.flush()?;

    let target = env::var("TARGET")?;
    Command::new("cargo")
        .arg("build")
        .arg("--target")
        .arg(&target)
        .arg("--manifest-path")
        .arg(project_cargo_path)
        .arg("--target-dir")
        .arg(&outdir)
        .current_dir(&project_path)
        .status()?;

    // we always use debug build for stub due to speed!
    println!(
        "cargo:rustc-link-search={}",
        format!("{}/{}/{}", outdir, target, "debug")
    );
    println!("cargo:rustc-link-lib=binder_ndk");

    Ok(())
}

fn main() {
    println!("cargo:rerun-if-changed=src/BinderBindings.hpp");
    println!("cargo:rerun-if-changed=src/wrapper.h");
    println!("cargo:rerun-if-changed=src/symbols.txt");

    build_stub().unwrap();

    // 自动设置 NDK 工具链符号链接（解决版本号问题）
    if let Some(ndk_path) = detect_android_ndk() {
        if let Err(e) = setup_toolchain_links(&ndk_path) {
            println!("cargo:warning=设置工具链链接失败: {}", e);
        }
    }

    // 构建 bindgen builder
    let mut builder = bindgen::Builder::default()
        .clang_arg("-Isrc/include_cpp")
        .clang_arg("-Isrc/include_ndk")
        .clang_arg("-Isrc/include_platform")
        .clang_arg("-target")
        .clang_arg("aarch64-linux-android")
        .clang_arg("-D__ANDROID_API__=33")
        .clang_arg("-D__ANDROID__")
        .default_enum_style(EnumVariation::Rust {
            non_exhaustive: true,
        })
        .constified_enum("android::c_interface::consts::.*")
        .allowlist_type("android::c_interface::.*")
        .allowlist_type("AStatus")
        .allowlist_type("AIBinder_Class")
        .allowlist_type("AIBinder")
        .allowlist_type("AIBinder_Weak")
        .allowlist_type("AIBinder_DeathRecipient")
        .allowlist_type("AParcel")
        .allowlist_type("binder_status_t")
        .allowlist_function(".*")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));

    // 添加 Android NDK 包含路径
    if let Some(include_paths) = setup_ndk_include_paths() {
        let ndk_home = detect_android_ndk().unwrap();
        let host_tag = get_host_tag();
        let sysroot = ndk_home.join(format!("toolchains/llvm/prebuilt/{}/sysroot", host_tag));
        
        // 添加 sysroot
        if sysroot.exists() {
            builder = builder.clang_arg(format!("--sysroot={}", normalize_path_for_clang(&sysroot)));
        }
        
        // 首先添加 clang 内置头文件路径（确保标准类型定义）
        for path in &include_paths {
            if path.to_string_lossy().contains("clang") && path.to_string_lossy().contains("include") {
                let normalized_path = normalize_path_for_clang(path);
                builder = builder.clang_arg(format!("-I{}", normalized_path));
                println!("cargo:warning=Adding clang include path: {}", normalized_path);
            }
        }
        
        // 然后添加其他包含路径
        for path in include_paths {
            if !path.to_string_lossy().contains("clang") || !path.to_string_lossy().contains("include") {
                let normalized_path = normalize_path_for_clang(&path);
                builder = builder.clang_arg(format!("-I{}", normalized_path));
                println!("cargo:warning=Adding include path: {}", normalized_path);
            }
        }
    }

    // 处理额外的 clang 参数
    let extra_args = collect_extra_clang_args();
    for arg in extra_args {
        builder = builder.clang_arg(arg);
    }

    // 添加基本的 C 类型定义以避免 size_t 等类型找不到的问题
    builder = builder
        .clang_arg("-D_GNU_SOURCE")
        .clang_arg("-D__STDC_LIMIT_MACROS")
        .clang_arg("-D__STDC_CONSTANT_MACROS")
        .clang_arg("-D__STDC_FORMAT_MACROS")
        .clang_arg("-target")
        .clang_arg("aarch64-linux-android33")
        .clang_arg("-fno-addrsig")
        .clang_arg("-include")
        .clang_arg("src/types_workaround.h")
        .header("src/BinderBindings.hpp");

    // 生成绑定
    let bindings = builder
        .generate()
        .expect("Unable to generate bindings");

    // 输出绑定文件
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");
}

/// 设置 Android NDK 包含路径
fn setup_ndk_include_paths() -> Option<Vec<PathBuf>> {
    let ndk_home = detect_android_ndk()?;
    println!("cargo:warning=Found Android NDK at: {}", ndk_home.display());
    
    let mut paths = Vec::new();
    
    // 对于 NDK r28b+，sysroot 在 toolchains/llvm/prebuilt/host-tag/sysroot
    let host_tag = get_host_tag();
    let sysroot_base = ndk_home.join(format!("toolchains/llvm/prebuilt/{}/sysroot", host_tag));
    
    if sysroot_base.exists() {
        // 基础系统头文件
        paths.push(sysroot_base.join("usr/include"));
        
        // 架构特定头文件
        if let Ok(target) = env::var("TARGET") {
            let arch_include = match target.as_str() {
                t if t.contains("aarch64") => "aarch64-linux-android",
                t if t.contains("armv7") => "arm-linux-androideabi", 
                t if t.contains("i686") => "i686-linux-android",
                t if t.contains("x86_64") => "x86_64-linux-android",
                _ => "aarch64-linux-android", // 默认
            };
            paths.push(sysroot_base.join(format!("usr/include/{}", arch_include)));
        }
    } else {
        // 兼容旧版 NDK 结构
        paths.push(ndk_home.join("sysroot/usr/include"));
        
        if let Ok(target) = env::var("TARGET") {
            let arch_include = match target.as_str() {
                t if t.contains("aarch64") => "aarch64-linux-android",
                t if t.contains("armv7") => "arm-linux-androideabi", 
                t if t.contains("i686") => "i686-linux-android",
                t if t.contains("x86_64") => "x86_64-linux-android",
                _ => "aarch64-linux-android", // 默认
            };
            paths.push(ndk_home.join(format!("sysroot/usr/include/{}", arch_include)));
        }
    }
    
    // Clang 编译器内置头文件
    let toolchain_base = ndk_home.join("toolchains/llvm/prebuilt");
    let clang_include = toolchain_base
        .join(host_tag)
        .join("lib/clang");
    
    // 查找 clang 版本目录
    if let Ok(entries) = std::fs::read_dir(&clang_include) {
        for entry in entries.flatten() {
            if entry.file_type().map_or(false, |t| t.is_dir()) {
                let include_dir = entry.path().join("include");
                if include_dir.exists() {
                    paths.push(include_dir);
                    break;
                }
            }
        }
    }
    
    // 过滤存在的路径
    let existing_paths: Vec<PathBuf> = paths
        .into_iter()
        .filter(|p| {
            let exists = p.exists();
            if !exists {
                println!("cargo:warning=Path does not exist: {}", p.display());
            } else {
                println!("cargo:warning=Found include path: {}", p.display());
            }
            exists
        })
        .collect();
    
    if existing_paths.is_empty() {
        None
    } else {
        Some(existing_paths)
    }
}

/// 检测 Android NDK 安装路径
fn detect_android_ndk() -> Option<PathBuf> {
    // 1. 从环境变量获取
    let env_vars = ["ANDROID_NDK_HOME", "NDK_ROOT", "ANDROID_NDK_ROOT"];
    for var in &env_vars {
        if let Ok(path_str) = env::var(var) {
            let path = PathBuf::from(path_str);
            if path.exists() && is_valid_ndk(&path) {
                return Some(path);
            }
        }
    }
    
    // 2. 检查常见安装位置
    let common_paths = [
        "C:/Android/android-ndk-r28b",
        "D:/android-ndk-r28b", 
        "C:/android-ndk-r28b",
        "D:/Android/android-ndk-r28b",
    ];
    
    for path_str in &common_paths {
        let path = PathBuf::from(path_str);
        if path.exists() && is_valid_ndk(&path) {
            return Some(path);
        }
    }
    
    // 3. 检查 Android SDK 中的 NDK
    if let Ok(sdk_root) = env::var("ANDROID_SDK_ROOT") {
        let ndk_bundle = PathBuf::from(sdk_root).join("ndk-bundle");
        if ndk_bundle.exists() && is_valid_ndk(&ndk_bundle) {
            return Some(ndk_bundle);
        }
    }
    
    None
}

/// 自动设置工具链符号链接以解决版本号问题
fn setup_toolchain_links(ndk_path: &PathBuf) -> Result<()> {
    let host_tag = get_host_tag();
    let bin_dir = ndk_path.join(format!("toolchains/llvm/prebuilt/{}/bin", host_tag));
    
    if !bin_dir.exists() {
        println!("cargo:warning=工具链目录不存在: {}", bin_dir.display());
        return Ok(());
    }
    
    println!("cargo:warning=检查工具链符号链接: {}", bin_dir.display());
    
    // 目标架构列表
    let targets = [
        "aarch64-linux-android",
        "armv7a-linux-androideabi", 
        "i686-linux-android",
        "x86_64-linux-android",
    ];
    
    // 当前目标架构
    let current_target = env::var("TARGET").unwrap_or_default();
    let target_to_process = if targets.contains(&current_target.as_str()) {
        vec![current_target.as_str()]
    } else {
        targets.to_vec()
    };
    
    // 设置 clang 链接
    for target in &target_to_process {
        setup_target_toolchain_links(&bin_dir, target)?;
    }
    
    // 设置 llvm-ar 链接
    setup_ar_links(&bin_dir, &target_to_process)?;
    
    Ok(())
}

/// 为特定目标架构设置工具链链接
fn setup_target_toolchain_links(bin_dir: &PathBuf, target: &str) -> Result<()> {
    // 查找最新版本的 clang（模拟帖子中的方法）
    let entries = std::fs::read_dir(bin_dir)?;
    let mut clang_files = Vec::new();
    
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();
        
        // 匹配模式：target + 数字 + -clang + 可选扩展名
        if file_name_str.starts_with(target) && 
           file_name_str.contains("-clang") &&
           file_name_str.chars().nth(target.len()).map_or(false, |c| c.is_ascii_digit()) {
            clang_files.push(file_name_str.to_string());
        }
    }
    
    // 排序以获取最新版本（ls 的排列决定了最新版本的 clang 肯定是最后一个）
    clang_files.sort();
    
    if let Some(latest_clang) = clang_files.last() {
        let source_path = bin_dir.join(latest_clang);
        
        // 确定目标文件名（去掉版本号）
        let target_name = if latest_clang.ends_with(".exe") {
            format!("{}-clang.exe", target)
        } else if latest_clang.ends_with(".cmd") {
            format!("{}-clang.cmd", target)
        } else {
            format!("{}-clang", target)
        };
        
        let target_path = bin_dir.join(&target_name);
        
        if !target_path.exists() {
            println!("cargo:warning=创建工具链链接: {} -> {}", target_name, latest_clang);
            
            // 尝试创建符号链接，失败则复制文件
            if std::os::windows::fs::symlink_file(&source_path, &target_path).is_err() {
                std::fs::copy(&source_path, &target_path)?;
            }
        } else {
            println!("cargo:warning=工具链链接已存在: {}", target_name);
        }
    } else {
        println!("cargo:warning=未找到 {} 的 clang 编译器", target);
    }
    
    Ok(())
}

/// 设置 llvm-ar 链接
fn setup_ar_links(bin_dir: &PathBuf, targets: &[&str]) -> Result<()> {
    // 查找 llvm-ar
    let ar_candidates = ["llvm-ar.exe", "llvm-ar.cmd", "llvm-ar"];
    let mut found_ar = None;
    
    for candidate in &ar_candidates {
        let ar_path = bin_dir.join(candidate);
        if ar_path.exists() {
            found_ar = Some(candidate);
            break;
        }
    }
    
    if let Some(ar_name) = found_ar {
        let source_path = bin_dir.join(ar_name);
        
        for target in targets {
            let target_ar_name = if ar_name.ends_with(".exe") {
                format!("{}-ar.exe", target)
            } else if ar_name.ends_with(".cmd") {
                format!("{}-ar.cmd", target)
            } else {
                format!("{}-ar", target)
            };
            
            let target_ar_path = bin_dir.join(&target_ar_name);
            
            if !target_ar_path.exists() {
                println!("cargo:warning=创建 ar 链接: {} -> {}", target_ar_name, ar_name);
                
                // 尝试创建符号链接，失败则复制文件
                if std::os::windows::fs::symlink_file(&source_path, &target_ar_path).is_err() {
                    std::fs::copy(&source_path, &target_ar_path)?;
                }
            } else {
                println!("cargo:warning=ar 链接已存在: {}", target_ar_name);
            }
        }
    } else {
        println!("cargo:warning=未找到 llvm-ar");
    }
    
    Ok(())
}

/// 验证是否为有效的 NDK 安装
fn is_valid_ndk(path: &PathBuf) -> bool {
    // 检查新版 NDK 结构 (r28b+)
    let new_sysroot = path.join("toolchains/llvm/prebuilt").join(get_host_tag()).join("sysroot");
    let has_new_structure = new_sysroot.exists() && path.join("toolchains/llvm/prebuilt").exists();
    
    // 检查旧版 NDK 结构
    let has_old_structure = path.join("sysroot").exists() && path.join("toolchains/llvm/prebuilt").exists();
    
    has_new_structure || has_old_structure
}

/// 获取主机平台标识
fn get_host_tag() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows-x86_64"
    } else if cfg!(target_os = "linux") {
        "linux-x86_64"
    } else if cfg!(target_os = "macos") {
        "darwin-x86_64"
    } else {
        "windows-x86_64" // 默认
    }
}

/// 收集额外的 clang 参数
fn collect_extra_clang_args() -> Vec<String> {
    let mut args = Vec::new();
    
    // 标准环境变量
    if let Ok(extra) = env::var("BINDGEN_EXTRA_CLANG_ARGS") {
        args.extend(shell_words::split(&extra).unwrap_or_default());
    }
    
    // 目标架构特定的环境变量
    if let Ok(target) = env::var("TARGET") {
        // 尝试下划线格式
        let var_underscore = format!("BINDGEN_EXTRA_CLANG_ARGS_{}", target.replace("-", "_"));
        if let Ok(extra) = env::var(&var_underscore) {
            args.extend(shell_words::split(&extra).unwrap_or_default());
        }
        
        // 尝试连字符格式
        let var_dash = format!("BINDGEN_EXTRA_CLANG_ARGS_{}", target);
        if let Ok(extra) = env::var(&var_dash) {
            args.extend(shell_words::split(&extra).unwrap_or_default());
        }
    }
    
    args
}

/// 将 Windows 路径规范化为 clang 可接受的格式
fn normalize_path_for_clang(path: &PathBuf) -> String {
    let path_str = path.to_string_lossy();
    
    if cfg!(target_os = "windows") {
        // 处理 Windows 驱动器路径: C:\path -> /C/path
        if path_str.len() > 2 && path_str.chars().nth(1) == Some(':') {
            let drive = path_str.chars().nth(0).unwrap().to_ascii_uppercase();
            let rest = &path_str[2..].replace('\\', "/");
            format!("/{}{}", drive, rest)
        } else {
            path_str.replace('\\', "/")
        }
    } else {
        path_str.into_owned()
    }
}
