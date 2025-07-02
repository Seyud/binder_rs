#ifndef TYPES_WORKAROUND_H
#define TYPES_WORKAROUND_H

// 为 Android NDK 提供缺失的基本类型定义
// 这解决了在 Windows 下使用 bindgen 时找不到 size_t 等类型的问题

#ifndef _SIZE_T_DEFINED
#define _SIZE_T_DEFINED
#ifdef __LP64__
typedef unsigned long size_t;
typedef long ssize_t;
#else
typedef unsigned int size_t;
typedef int ssize_t;
#endif
#endif

#ifndef _STDINT_H
typedef unsigned int uint32_t;
typedef int int32_t;
#endif

#endif // TYPES_WORKAROUND_H
