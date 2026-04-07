前提：
编译 target：rustup target add x86_64-unknown-linux-musl
交叉编译链接器：brew install filosottile/musl-cross/musl-cross

打包：
运行：cargo build --release --target x86_64-unknown-linux-musl
产物在：target/x86_64-unknown-linux-musl/release/zeroclaw
