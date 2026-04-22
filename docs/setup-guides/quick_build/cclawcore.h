/*
 * cclawcore.h — C ABI for libcclawcore_ffi
 *
 * 对应 crate: crates/cclawcore-ffi
 * 产物: libcclawcore_ffi.so / .dylib / .dll
 *
 * 将本头文件与共享库一起分发给集成方（Android / Linux / macOS 等）。
 */

#ifndef CCLAWCORE_H
#define CCLAWCORE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * 启动 CclawCore daemon（在后台 tokio runtime 中运行）。
 *
 * @param config_dir  配置目录路径（UTF-8，必须非 NULL）。
 * @param host        绑定地址（UTF-8）。传 NULL 则使用配置文件中的默认值。
 * @param port        gateway 端口。传 0 则使用配置文件中的默认值。
 * @param sw_preset   是否启用内置的 Seewo 预设。
 *
 * @return  0  成功启动
 *          1  daemon 已在运行
 *         -1  出错（参数非法 / runtime 创建失败等，详见日志）
 */
int32_t cclawcore_start(const char *config_dir,
                        const char *host,
                        uint16_t port,
                        bool sw_preset);

/**
 * 停止正在运行的 CclawCore daemon。
 *
 * 阻塞直到所有组件完全关停。如果 daemon 未运行，则为 no-op。
 */
void cclawcore_stop(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* CCLAWCORE_H */
