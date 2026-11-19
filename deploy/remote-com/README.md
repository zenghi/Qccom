# QCCOM 1.9.8 远程 COM

两台 **Windows x64** 电脑各运行一份带“远程 COM”的 QCCOM。假设设备接在远端的 COM3，本地生成 COM20 ↔ COM21。QCCOM 桥接 COM20，烧录软件打开 COM21 即可操作远端 COM3。也可以用本地 QCCOM 的普通串口面板打开 COM21。两种用法轮流占用 COM21。

## 在自己的服务器上部署

准备 Linux、Docker Compose v2、Python 3 和 OpenSSL 1.1.1 或更新版本。将本目录复制到服务器，在该目录执行以下命令。示例域名换成服务器的公网 IPv4 或解析到它的域名。

```sh
python3 pair.py serial.example.com
docker compose up -d
docker compose logs --tail 20
```

防火墙和云安全组放行入站 **TCP 7000**。服务端固定使用 frps 0.72.0 镜像，负责配对、协调打洞，以及需要时的中继。串口服务只在设备机的回环地址监听。两台电脑需要能向服务器发起 TCP 连接。P2P 还需要能向 STUN 和对端发 UDP 包，端口随 NAT 分配。

默认使用 frp 的公共 STUN 服务 `stun.easyvoip.com:3478` 探测公网映射。如有自己的 STUN，可在首次生成时指定 `python3 pair.py serial.example.com --stun stun.example.com:3478`。frp 需要服务器返回可访问的备用地址（RFC 5780 OTHER-ADDRESS 或 CHANGED-ADDRESS）。无法取得映射或两端 NAT 不兼容时，新串口连接会在尝试 P2P 约 2.5 秒后转到 STCP 中继。已经打开的串口流在本次连接中保持原路径。

将生成的 **pair/host.zip** 私下交给设备机，**pair/client.zip** 交给烧录机。两端各自解压，保持 JSON 和 server.crt 在同一目录。配对包包含这对电脑的访问凭据。此部署面向一对相互信任的电脑，证书有效期两年。服务器备份整个 pair 目录；重启只需 `docker compose up -d`。轮换时先 `docker compose down`，移走旧 pair，重新生成并分发两份新包，再启动服务。

## 两台 Windows 电脑的一次性准备

1. 将本版本的 qccom.exe 和 prepare-windows.ps1 放在同一目录。在设备机运行 `.\prepare-windows.ps1`。脚本校验 exe 旁 remote-com 子目录里的 frpc 与 hub4com。使用已附这两个工具的测试包时可直接启动设备端；缺少工具时脚本会下载固定版本并核对 SHA-256。
2. 在烧录机安装已签名并能在当前 Windows 加载的 com0com 驱动，然后在**管理员 PowerShell** 中运行 `.\prepare-windows.ps1 -CreatePair`。脚本增建一对空闲 COM 号，并列出配置。记住新建的 A、B 端；重复使用时保留原端口对。若驱动已安装但脚本找不到 setupc.exe，可直接在 com0com 的 Setup Command Prompt 执行下面的命令，然后单独运行 prepare-windows.ps1。
3. QCCOM 日常使用以普通用户运行。串口驱动只需在烧录机安装。

```text
install PortName=COM#,ri=ropen PortName=COM#,EmuBR=yes
list
```

驱动与工具来源：[com0com 3.0](https://sourceforge.net/projects/com0com/files/com0com/3.0.0.0/)、[hub4com 2.1](https://sourceforge.net/projects/com0com/files/hub4com/2.1.0.0/)、[frp 0.72.0](https://github.com/fatedier/frp/releases/tag/v0.72.0)。

## 调试或烧录

1. **设备机**在串口侧栏展开“远程 COM（P2P）”，选 host.json、板子对应的物理 COM，点“启动映射”。先断开普通串口面板和其他占用该物理口的软件。部分 USB 转串口在探测或打开时会短暂改变 DTR/RTS。
2. **烧录机**在同一入口选 client.json、刚创建的 A ↔ B 端口对，点“启动映射”。默认提前打开远端口，等界面显示“远程串口就绪 · P2P”或“远程串口就绪 · 服务器中继”。
3. **烧录软件**选择界面提示的 B 端（例 COM21），按板子的常规流程识别、烧写和校验。QCCOM 的普通串口面板也可以独占这个 B 端进行收发、切换 DTR/RTS 或发送 Break。使用烧录软件前先关闭普通串口面板中的 B 端，保持远程映射运行。
4. 若板子依靠“打开/关闭物理串口”进入引导程序，启动映射前勾选“随应用打开和关闭远端串口”。此时等待烧录工具打开 B 端才连接设备。这个模式依赖新端口对 A 端的 `ri=ropen` 配置。对打开后立即发复位脉冲的工具，默认的提前连接模式能避开首次建链等待。
5. 结束时在任一端点“停止映射”。退出 QCCOM 时，Windows Job Object 会关闭该实例启动的辅助进程。

## 能力与验收

通过 RFC 2217 在同一条有序流中传递二进制数据、波特率、数据位、校验、停止位、DTR、RTS 和 Break。物理控制线能力取决于 USB 转串口芯片与接线。公网往返时延和抖动会影响握手超时和复位脉宽。此功能针对 UART 烧录，依赖 USB 重枚举、原生 USB DFU、SWD 或 JTAG 的设备需要对应的调试接口。

| 检查 | 操作与预期 |
| --- | --- |
| 端口映射 | 先在设备端接 TX/RX 回环，用本地 QCCOM 的 HEX 模式发送含 00、FF 的字节，确认原样返回。 |
| 参数与复位 | 用已知正常的串口板先读取芯片 ID，在 115200 和烧录工具的高速阶段分别通信；有条件时用逻辑分析仪对照 DTR、RTS、Break 和设备侧波特率。 |
| 烧写结果 | 先使用可恢复的测试板和已知镜像，检查烧录工具的读回校验，再复位观察固件启动。 |
| NAT 与中继 | 临时限制客户端的出站 UDP，重新启动映射，检查显示服务器中继并重复回环测试。 |
| 中断与释放 | 传输中拔掉设备或断网；检查烧录工具报错。停止映射，恢复后重新读取芯片 ID 并从头烧写。禁止在烧录时由第二个程序向同一设备写入。 |

**故障定位。** 没有端口对时检查 `setupc list` 和设备管理器，重新选择 client.json 刷新。若出现 CreateFile / Access denied，核对 A/B 端及串口占用。证书或登录失败时检查配对包是否匹配、两端系统时间及 TCP 7000。烧录工具超时而回环正常时，先用提前连接模式、较长的握手超时和较低的烧录波特率重试。两端 QCCOM 的“连接日志”和服务器的 `docker compose logs --tail 100` 可用于区分串口错误和隧道错误。
