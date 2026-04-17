前提：
编译target：rustup target add aarch64-linux-android
NDK工具链：安装 Android NDK，确保 aarch64-linux-android21-clang 在 PATH 中
  - 下载：https://developer.android.com/ndk/downloads
  - 设置PATH：export PATH=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin:$PATH

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
