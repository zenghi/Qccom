# QCCOM 端口映射

两台 Windows x64 电脑各运行一个 `qccom-portable.exe`，一台 Linux x86_64 公网服务器运行一个 Rust 可执行文件 `qccom-server`。串口、MQTT 和被动 FTP 共用经过身份认证的 QUIC 端点，可以同时开启。接开发板或运行 MQTT、FTP 服务的电脑称为设备端，运行烧录工具或网络客户端的电脑称为使用端。

## 部署公网服务器

把域名的 A 记录指向服务器。防火墙和云安全组放行 **TCP 7000**（会合及加密中继）和 **UDP 7842**（公网地址探测）。使用 CDN 的域名需选用能直达这两个端口的 DNS 记录。为服务器域名申请客户端信任的公网 TLS 证书，准备 PEM 格式的完整证书链和私钥。解压部署包后可直接前台启动。

```sh
chmod +x qccom-server
./qccom-server serve --cert /path/to/fullchain.pem --key /path/to/privkey.pem
```

设备端在「高级设置 → 穿透服务器」填写运行中继服务且证书覆盖的域名，默认值为 `www.zenghi.com`。使用公网 IP 时，证书的 SAN 须包含该 IP。点击「测试服务器」可从当前电脑验证 TLS 和中继注册。设备端生成的分享地址会自动带上服务器地址，使用端粘贴后无需另行配置服务器。

服务器已启动但分享超时时，在发起分享的 Windows 电脑上运行以下命令，域名换成 QCCOM 中实际填写的地址。

```powershell
Test-NetConnection www.zenghi.com -Port 7000
curl.exe --noproxy "*" --connect-timeout 8 https://www.zenghi.com:7000/healthz
```

TCP 测试失败时检查服务器安全组、防火墙和客户端网络。HTTPS 返回证书错误时检查证书的域名和完整链。HTTPS 正常时继续用 QCCOM 的「测试服务器」检查 WebSocket 和中继注册。

可访问公网服务器的 QCCOM 设备可以注册共享。服务端对每条中继连接的接收流量默认限速 8 MiB/s，允许 1 MiB 突发。设置 `QCCOM_RELAY_BYTES_PER_SECOND` 可调整速率，单位为字节/秒，最小值为 10。限速只作用于中继路径。

### 开机自启

部署包中的 systemd 单元从 `/etc/qccom` 读取证书和私钥，并由 systemd 将两份文件提供给动态服务账户。把命令中的证书来源路径换成实际路径。

```sh
sudo install -m 0755 qccom-server /usr/local/bin/qccom-server
sudo install -m 0644 qccom-server.service /etc/systemd/system/qccom-server.service
sudo install -d -m 0700 /etc/qccom
sudo install -m 0600 /path/to/fullchain.pem /etc/qccom/fullchain.pem
sudo install -m 0600 /path/to/privkey.pem /etc/qccom/privkey.pem
sudo systemctl daemon-reload
sudo systemctl enable qccom-server
sudo systemctl restart qccom-server
sudo journalctl -u qccom-server -n 20
```

证书续期后更新 `/etc/qccom` 下的文件并重启服务。也可直接从证书管理器的目录加载证书。用 `sudo systemctl edit qccom-server` 写入以下配置，并将示例路径改成实际路径。证书续期后重启服务即可加载新证书。

```ini
[Service]
LoadCredential=
LoadCredential=relay-fullchain.pem:/etc/letsencrypt/live/www.zenghi.com/fullchain.pem
LoadCredential=relay-privkey.pem:/etc/letsencrypt/live/www.zenghi.com/privkey.pem
```

需要自定义中继速率时，在 `/etc/default/qccom-server` 写入速率设置并重启服务。

```ini
QCCOM_RELAY_BYTES_PER_SECOND=8388608
```

Windows 便携模式把设置保存在 EXE 同目录的 `qccom-data`。把程序和此目录放在当前用户专有的可写位置。

## 在设备列表中使用

| 用途 | 设备端操作 | 使用端操作 |
| --- | --- | --- |
| 串口烧录 | 点实体 COM 行右侧的「分享到网络」，再点「复制分享地址」 | 点「关联网络分享」并粘贴地址。烧录工具选择窗口显示的本机虚拟 COM |
| MQTT | 点「分享到网络」并选择 MQTT，填写 broker 的主机和端口，默认 `127.0.0.1:1883`，复制分享地址 | 关联网络分享后，MQTT 客户端连本机 `127.0.0.1:1883`。认证和消息照常交给 broker 处理 |
| FTP | 点「分享到网络」并选择 FTP，目标默认 `127.0.0.1:21`。在 FTP 服务配置被动端口 `50000–50009`，或改成服务实际范围，再复制分享地址 | 关联网络分享后，使用被动 FTP，默认连本机 `127.0.0.1:2121`。两端自动协商数据端口范围 |

连接地址形如 `qccom://域名:7000/…`，包含该映射的授权。只需复制和粘贴完整地址，两端自动完成握手，无需导入 JSON 或额外输入配对码。地址可在生成后 15 分钟内用于建立连接，请仅发给要连接的人。停止共享或退出设备端 QCCOM 后，该地址失效。到期时，等待连接的共享会自动停止。已有连接可以继续使用，结束后可点「重新共享」生成新地址。

使用端只在回环地址监听。同机另一个程序已占用端口时，更改「本机监听端口」；FTP 被动范围同样需要空闲。普通 FTP 的 PASV 和 EPSV 都受支持。FTP 协议代理处理明文控制命令，隧道使用端到端加密。主动 FTP、显式 FTPS 和隐式 FTPS 均在本版支持范围之外。

连接地址已标识对应映射。多个映射可以共用同一个公网 IP。串口和 MQTT/FTP 的业务端口由两端 QCCOM 管理，公网服务器只需开放 TCP 7000 和 UDP 7842。

COM 分享连通后，两端可从映射卡片的「查看日志」打开对应串口。TX 表示发送给实体设备的数据，RX 表示从该设备读出的数据。两端都使用支持日志同步的客户端时，设备端在 QCCOM 中发送的数据也会同步到使用端的 TX 日志。日志同步由客户端协商。

### 直连与中继

两端先通过服务器交换连接信息并验证授权。QCCOM 优先建立直连，打洞受限时回退到加密中继。界面根据实际选定路径显示「已直连」或「服务器中继」。网络变化后 QUIC 会更新路径，电脑掉线或映射被停止时释放连接。本机监听端口冲突或对端未共享对应映射时，先改正原因再连接。

使用 QCCOM 连接地址时，MQTT 客户端连本机 QCCOM 监听端口，直连成功后业务数据直接传给设备端 QCCOM，再交给 MQTT broker。只有回退到中继时，业务数据才经过 QCCOM 公网服务器。普通 MQTT 客户端或浏览器直接访问某台公网服务器的业务 IP 和端口，数据会到达该地址对应的服务或转发代理。本版本的共享服务支持 MQTT 和被动 FTP，HTTP 端口共享尚未提供。

### UART 烧录

使用端首次需要 Windows 可加载的 com0com 系统驱动。已有虚拟口对可直接使用。「准备虚拟串口」会检查现有端口和驱动，必要时下载校验过的驱动并请求 Windows 管理员确认。设备端直接打开实体口。实体口与虚拟口在同一映射行并排显示，应用端口可以由外部烧录软件或 QCCOM 的普通串口功能独占打开。

如果设备管理器显示代码 52，Windows 拒绝加载当前 com0com 驱动，虚拟 COM 口无法生成。QCCOM 会在「准备虚拟串口」显示这个原因。内置下载指向带旧式交叉签名的 com0com 3.0.0.0 原版安装包。需要在当前 Windows 上成功加载的、支持 com0com 控制线接口的驱动，才能使用虚拟 COM 烧录。[Microsoft 代码 52 说明](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/cm-prob-unsigned-driver)

新映射路径由 Rust 转发串口字节、波特率、数据位、校验、停止位、DTR、RTS 和 Break。设备可用的波特率和线控由 USB 转串口驱动决定。跨网复位脉宽还受电脑调度和网络抖动影响。首次建议执行芯片识别，再烧录带读回校验的镜像。掉线时检查设备并从头开始烧录。支持串口 bootloader，USB DFU、SWD、JTAG 需要各自的接口。

「共享给本机软件」用于同一台电脑上的本地虚拟串口共享，可在已连接的设备行操作。

## 从源码构建

Windows 使用稳定版 Rust 构建，将生成的 `target/release/qccom.exe` 改名为 `qccom-portable.exe` 启用便携存储。新映射使用默认 Cargo 功能。

```sh
cargo build --locked --release
```

x86_64 Linux 静态单文件需要 musl C 工具链。

```sh
rustup target add x86_64-unknown-linux-musl
CC_x86_64_unknown_linux_musl=musl-gcc CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc cargo build --locked --release -p qccom-tunnel --features server --bin qccom-server --target x86_64-unknown-linux-musl
```

网络回归测试在本机启动中继并建立共享连接。

```sh
cargo test --locked -p qccom-tunnel --features endpoint,server --lib
```
