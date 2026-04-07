前提：
编译 target：rustup target add x86_64-pc-windows-gnu
交叉编译工具链：brew install mingw-w64
PATH 配置：echo 'export PATH="/opt/homebrew/opt/mingw-w64/bin:$PATH"' >> ~/.zshrc && source ~/.zshrc

打包：
运行：cargo build --release --target x86_64-pc-windows-gnu
产物在：target/x86_64-pc-windows-gnu/release/zeroclaw.exe
