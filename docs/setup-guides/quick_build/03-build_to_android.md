前提：
编译target：rustup target add aarch64-linux-android
NDK工具链：安装 Android NDK，确保 aarch64-linux-android21-clang 在 PATH 中
  - 下载：https://developer.android.com/ndk/downloads
  - 设置PATH：export PATH=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin:$PATH

## 方式一：可执行文件（Termux / adb shell）

打包（ARM64，现代安卓手机）：
运行：cargo build --release --target aarch64-linux-android
产物在：target/aarch64-linux-android/release/cclawcore

打包（ARMv7，旧款安卓手机）：
编译target：rustup target add armv7-linux-androideabi
运行：cargo build --release --target armv7-linux-androideabi
产物在：target/armv7-linux-androideabi/release/cclawcore

部署（Termux）：
adb push target/aarch64-linux-android/release/cclawcore /data/local/tmp/
在Termux中：cp /data/local/tmp/cclawcore ~/ && chmod +x ~/cclawcore

## 方式二：共享库（.so，供 Android App 集成）

打包：
运行：cargo build -p cclawcore-ffi --release --target aarch64-linux-android
产物在：target/aarch64-linux-android/release/libcclawcore_ffi.so

配套头文件：`docs/setup-guides/quick_build/cclawcore.h`（与 `.so` 一起分发给集成方）。

导出的 C ABI 函数：

  int32_t cclawcore_start(
      const char* config_dir,   // 配置目录路径（UTF-8，不可为 NULL，必须对 App 进程可写）
      const char* host,         // 绑定地址（UTF-8，NULL 则使用配置默认值）
      uint16_t port,            // 端口号（0 则使用配置默认值）
      bool sw_preset            // 是否启用 Seewo 预设
  );
  // 返回：
  //    0  gateway 已绑定端口，daemon 运行中
  //    1  daemon 已经在本进程内运行（重复调用）
  //   -1  参数非法 / tokio runtime 创建失败 / FFI 入口 panic
  //   -2  daemon 初始化或 gateway bind 失败，或 30s 超时未就绪
  //        （具体原因见 logcat tag "cclawcore" 或 stderr）

  void cclawcore_stop();
  // 阻塞直到 daemon 完全关停；未运行时为 no-op

部署：
- 将 libcclawcore_ffi.so 放入 Android 项目的 jniLibs/arm64-v8a/ 目录，通过 JNA 或 NDK 调用。
- **cclawcore_start 是同步阻塞调用**，内部最长等 ~30s 直到 gateway bind 完成；Android 端必须
  在后台线程或协程里调用，避免阻塞 UI 线程触发 ANR。
- `config_dir` 推荐使用 `context.getFilesDir()` 下的子目录（如 `<filesDir>/cclawcore`），
  避免 Android 10+ 作用域存储导致的写入失败。
- 日志走 logcat（tag `cclawcore`），过滤规则可通过 `RUST_LOG` 环境变量调整，
  缺省为 `info,cclawcorelabs=debug,cclawcore_ffi=debug`。
- daemon 生命周期与 App 进程绑定：App 被系统杀掉时 daemon 也会退出。
  想在后台长期运行，应把调用方放进 Foreground Service 并持有前台通知。
- App 的 `onDestroy` / `onTerminate` 里请调用 `cclawcore_stop()`，
  让 tokio 工作线程和状态文件干净收尾。
