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

导出的 C ABI 函数：

  int32_t cclawcore_start(
      const char* config_dir,   // 配置目录路径（UTF-8，不可为 NULL）
      const char* host,         // 绑定地址（UTF-8，NULL 则使用配置默认值）
      uint16_t port,            // 端口号（0 则使用配置默认值）
      bool sw_preset            // 是否启用 Seewo 预设
  );
  // 返回：0 成功，1 已在运行，-1 错误

  void cclawcore_stop();
  // 阻塞直到 daemon 完全关停

部署：
将 libcclawcore_ffi.so 放入 Android 项目的 jniLibs/arm64-v8a/ 目录
通过 JNA 或 NDK 调用 cclawcore_start / cclawcore_stop
