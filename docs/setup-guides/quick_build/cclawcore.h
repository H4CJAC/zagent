/*
 * cclawcore.h — C ABI for libcclawcore_ffi
 *
 * 对应 crate: crates/cclawcore-ffi
 * 产物: libcclawcore_ffi.so / .dylib / .dll
 *
 * 将本头文件与共享库一起分发给集成方（Android / Linux / macOS 等）。
 *
 * 关键集成注意事项
 * ------------------
 * 1. cclawcore_start 是**同步阻塞**调用，内部最长等待 ~30 秒，直到 HTTP
 *    gateway 真正绑定监听端口（或失败）再返回。Android 等带有 ANR 检测
 *    的平台必须在后台线程/协程中调用，不要在 UI 线程里调。
 * 2. daemon 的 tokio 线程池与调用方共享同一进程：App 被系统杀掉时
 *    daemon 也会一并退出，并不存在独立的守护进程。正常退出请务必调用
 *    cclawcore_stop，以便让组件干净关停、flush 状态文件。
 * 3. 日志输出：Android 走 logcat（tag = "cclawcore"），其它平台走
 *    stderr。过滤规则从 RUST_LOG 读取，缺省等同于
 *    "info,cclawcorelabs=debug,cclawcore_ffi=debug"。
 */

#ifndef CCLAWCORE_H
#define CCLAWCORE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/**
 * 启动 CclawCore daemon 并阻塞直到 gateway 就绪或失败。
 *
 * @param config_dir  配置目录路径（UTF-8，必须非 NULL，且必须对当前
 *                    进程可写；Android 推荐使用
 *                    `context.getFilesDir()` 下的子目录）。
 * @param host        绑定地址（UTF-8）。传 NULL 则使用配置文件默认值。
 * @param port        gateway 端口。传 0 则使用配置文件默认值。
 * @param sw_preset   是否启用内置的 Seewo 预设。
 *
 * @return
 *   0  gateway 已绑定监听端口，daemon 运行中。
 *   1  daemon 已经在本进程内运行（重复调用）。
 *  -1  参数非法 / tokio runtime 创建失败 / FFI 入口捕获到 panic。
 *  -2  daemon 初始化或 gateway bind 失败，或 30 秒内未完成启动。
 *      具体原因请查看 logcat（tag "cclawcore"）或 stderr。
 */
int32_t cclawcore_start(const char *config_dir,
                        const char *host,
                        uint16_t port,
                        bool sw_preset);

/**
 * 停止正在运行的 CclawCore daemon。
 *
 * 阻塞直到所有组件完全关停。如果 daemon 未运行，则为 no-op。
 * 推荐在 App 的 onDestroy / onTerminate 钩子里调用，避免 .so 留下
 * 悬垂的 tokio 工作线程。
 */
void cclawcore_stop(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* CCLAWCORE_H */
