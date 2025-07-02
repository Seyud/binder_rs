/*
 * Wrapper header to ensure proper inclusion order for Android Binder bindings
 */

// 首先包含基本的 C 类型定义
#include <stddef.h>
#include <stdbool.h>
#include <stdint.h>
#include <sys/types.h>

// 然后包含 Android Binder 头文件
#include "BinderBindings.hpp"
