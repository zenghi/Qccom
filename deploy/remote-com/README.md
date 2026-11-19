# 单文件远程 COM

在两台 Windows x64 电脑上各放一份 `qccom-portable.exe`。设备机接开发板，烧录机通过系统虚拟 COM 使用本地烧录软件。Linux x86_64 服务器只运行 `qccom-server`。客户端内置 Rust QUIC 传输和串口适配器，服务端使用 Rust 的 iroh-relay 提供配对节点的会合、中继及公网地址探测。

## 部署服务器

将 `qccom-server` 传到服务器上一个属于服务账号的目录。在该目录执行以下命令，将示例地址换成两端均能访问的服务器公网 IPv4 或 DNS 名。初始化和运行均使用普通用户权限。

```sh
chmod +x qccom-server
./qccom-server init serial.example.com
./qccom-server serve
```

防火墙和云安全组放行 **TCP 7000**（TLS 会合与中继）和 **UDP 7842**（公网地址探测）。直连还需要两台电脑能相互发送 UDP。两端通过固定的服务器证书注册，再用对方的公钥验证 QUIC 连接。连接可先经服务器建立，发现可用直连路径后会自动切换；受限网络可持续走中继。串口流在任一路径上都经过端到端加密。

首次 init 生成 `qccom-pair/`，里面有三份私密配置：

| 文件 | 放置位置 |
| --- | --- |
| server.json | 留在服务器，由 serve 读取 |
| host.json | 私下交给接开发板的设备机 |
| client.json | 私下交给运行烧录工具的电脑 |

重启时在原目录执行 `./qccom-server serve`。如用 systemd 托管，将 WorkingDirectory 指向该目录，ExecStart 指向可执行文件并带 serve 参数。备份整个 qccom-pair 目录。初始化会拒绝覆盖已有配置；更换地址或轮换凭据时先停止服务并移走旧目录，再初始化和分发新文件。服务端允许这两个端点身份登录，设备端只接受与其配对的烧录端。

## 使用 Windows 便携版

1. 将 `qccom-portable.exe` 放在可写目录。两端均可直接运行。该文件名启用便携存储，设置放在旁边的 qccom-data 目录。更新便携版时退出程序，用新版同名 EXE 替换，保留设置和配对文件。
2. 设备机进入串口侧栏的 **远程 COM（P2P）**，打开 host.json，选择板子使用的物理串口并启动映射。先释放其他程序占用的同一物理口。部分 USB 转串口在打开时会短暂改变 DTR/RTS。
3. 烧录机首次使用时安装与本机 Windows 兼容的签名 com0com 驱动。在 QCCOM 选择 client.json，点击 **创建烧录串口对** 并接受 Windows 管理员提示。已有按下述方式配置好的端口对可以复用。设备机免装此虚拟串口驱动。
4. 选取虚拟串口对并启动映射。等“远程串口就绪”后，在烧录工具或本地 QCCOM 普通串口面板打开界面提示的应用端。例如 A=COM20、B=COM21 时，映射占用 COM20，烧录软件打开 COM21。普通串口面板与烧录软件轮流使用 COM21。
5. 默认提前建立连接以便烧录工具打开后立即发起复位。依靠串口打开/关闭触发动作的板子，可以在启动前勾选“随应用打开和关闭远端串口”。专用端口对的 A 端使用 ri=ropen，B 端使用 EmuBR=yes。
6. 完成后点击“停止映射”。启动映射时，便携版将内置 hub4com 释放到用户临时目录，停止映射后回收。传输进程和串口进程随 QCCOM 退出。配对配置须单独保管。

**适用范围。** RFC 2217 传递串口数据、波特率、数据位、校验、停止位、DTR、RTS 和 Break。公网抖动会影响复位脉宽和握手超时。首次使用应先读芯片 ID，再写入已知镜像并运行烧录工具的读回校验。断线或拔线后重新启动映射、重新识别芯片并从头烧录。USB DFU、SWD 和 JTAG 需要各自的调试接口。

**迁移旧测试版。** 原 frps 如果正在使用 TCP 7000，先在旧部署目录执行 `docker compose down` 并保留配置。Rust 服务使用 init 生成的新配对文件。原 frp 配置仍可搭配原服务器和原 remote-com 工具目录使用。[旧版部署说明](LEGACY-FRP.md)

## 构建与验证

使用当前稳定版 Rust 和仓库的 Cargo.lock。在 Windows 构建便携版，然后将生成的 target/release/qccom.exe 命名为 qccom-portable.exe。

```sh
cargo build --locked --release --features portable-remote-com
```

在 Linux 上构建服务。发布的 x86_64 Linux 单文件使用 musl 静态链接，构建机需安装 musl C 工具链。

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --locked --release -p qccom-tunnel --features server --bin qccom-server --target x86_64-unknown-linux-musl
```

在 Linux 上运行网络回归测试（需要本机 TCP 7000、UDP 7842 空闲），验证配对、直连、强制中继、二进制双向传输及端口重新打开：

```sh
cargo build --locked -p qccom-tunnel --features server,endpoint --bin qccom-server --example tunnel-worker
python3 tests/remote_com_rust.py target/debug/qccom-server target/debug/examples/tunnel-worker
```

便携测试版未签名，内置独立的 hub4com 2.1.0.0。运行 `qccom-portable.exe --hub4com-source 空目录` 可以导出对应源码及许可证。常规签名发布继续使用默认 Cargo 功能。[hub4com 来源](../../vendor/hub4com/NOTICE.txt)
