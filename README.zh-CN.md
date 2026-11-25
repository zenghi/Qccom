<p align="center">
  <img src="docs/logo.png" width="72" alt="QCCOM logo">
</p>

<h1 align="center">QCCOM 串口助手</h1>

<p align="center">
  给嵌入式开发者的开源串口调试工具。接上开发板就能看彩色日志、收发 HEX 或文本指令。<br>
  支持 TCP、UDP、MQTT 和 BLE，自带串口终端、代码浏览器和 Git 历史查看器。
</p>

<p align="center">
  <a href="https://github.com/zenghi/QCCOM/releases"><img alt="Release" src="https://img.shields.io/github/v/release/zenghi/QCCOM?color=2dd4bf"></a>
  <a href="https://github.com/zenghi/QCCOM/actions"><img alt="Build" src="https://img.shields.io/github/actions/workflow/status/zenghi/QCCOM/build.yml?branch=main"></a>
  <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <img alt="Platforms" src="https://img.shields.io/badge/platform-Windows%20%7C%20Linux-555">
</p>

<p align="center">
  <a href="README.md">English</a> · 简体中文
</p>

## Windows 端口映射

在实体 COM 行点击端口映射，复制生成的连接地址。另一台电脑把地址粘贴到 QCCOM 即可握手，无需导入凭据。握手完成后，设备列表会显示烧录工具应选的本机虚拟 COM。MQTT 和被动 FTP 也可用地址连接。QCCOM 优先直连，打洞失败时使用 Rust 服务器中继。地址共享要求服务器使用受信任的公网 TLS 证书。[部署与烧录指南](deploy/remote-com/README.md)

## 采集分析与 AI 连接

在「调试 → ⋯ → 采集分析」打开当前设备的工作台。FireWater 数值行（如 `d:1500,1498,2.1` 加换行）和 JustFloat 小端浮点数组可直接出图。RawData 用于普通串口日志。在「配置」里选择采样协议、给通道命名或设置阈值。点击图面依次放置 A、B 游标。FFT 使用设定的固件采样率，批量接收的数据以主机到达时间定位，包内按采样间隔展开。

「在线调参」可保存多个带 `{value}` 的发送模板。启用 SLCAN 后，可载入 DBC 观察 CAN 信号与串口日志的同一时间线。录制完成后可拖动回放、对照另一份采集，或从「文件」导出 CSV、MDF4、QCAP。QCAP 保留事件、参数和关联固件标识。内存窗口每通道保留最近 65,536 点，最多 64 个通道。长时间运行请先选择「持续写盘」写入新的 QREC 文件，再选择时间段载入。MDF4 支持带时间主通道的已排序数值组、整数和浮点、线性换算、DT/DL/HL 数据及 Deflate 压缩；单文件和单组展开数据上限为 128 MiB。

「调试 → ⋯ → AI 连接」可临时共享当前会话，复制配置到支持 MCP stdio 的客户端。AI 可查看当前工程、工具链与编译问题，按时间读取事件及波形摘要，比较两次采集。录制/已保存设备指令与编译/烧录分别授权。关闭共享后本次配置失效。选「关联固件文件」会把文件 SHA-256 记入采集，便于比较不同固件的结果。

再次双击 EXE 会打开另一个进程，可独立选择设备和保存窗口布局。右键打开文件仍可送到第一个窗口。ESP-IDF 已有用户实机验证，其他 MCU 工具链继续标为未测试。

## 图文上手

- **串口调试**　用过 SSCOM32？用文本和 HEX 各发一次 `AT`，看设备是否回复 `OK`。[串口助手收发教程](docs/serial-debug.zh-CN.md) · [SSCOM32 常用操作对照](https://www.zenghi.com/switch-from-sscom/)
- **MQTT 工具**　在本机订阅温度主题、发布一条读数，再到接收日志核对消息。[MQTT 发布与订阅图解](docs/mqtt-tool.zh-CN.md)
- **代码对比工具**　在两版固件中找出改动的文件，再逐行核对 UART 波特率。[文件和文件夹对比图解](docs/code-compare.zh-CN.md)

![QCCOM 以 115200 波特率连接设备，中间按颜色区分警告和错误日志，右侧列出 AT 快捷命令](docs/screenshot.png)

QCCOM 用 Rust + [egui](https://github.com/emilk/egui) 编写，只有一个可执行文件（Windows 版约 8 MB），免安装、不需要运行库、不需要管理员权限。启动不到一秒，几十万行日志也能流畅滚动。

## 主要功能

- **串口即插即用**：每秒自动扫描，显示芯片型号和 VID:PID（如 `COM5 · CH340 (1a86:7523)`）；新插入的设备自动选中，拔掉后重新插上会自动重连。
- **自动识别波特率**：依次尝试 18 种常用波特率，给收到的数据打可读性分，选最好的自动连接。
- **Linux 终端风格日志**：ANSI 颜色（16 / 256 / 真彩色）；`[ERROR]`、`E (123) tag:`、`E/tag:`、`<err>` 等格式自动按级别着色；毫秒时间戳、关键词高亮、搜索、按级别过滤、文本 / HEX / 混合视图。
- **多种编码**：UTF-8、GBK / GB2312、ASCII、Latin-1，切换后历史日志立即按新编码重新显示。
- **完整的终端模式**：VT100 / xterm 仿真，直接操作 U-Boot 和 Linux shell；vi、top、less、menuconfig 都能正常显示；支持回滚历史、鼠标选择复制、粘贴。
- **网络连接，可同时在线**：TCP 客户端、TCP 服务器（可多客户端）、UDP、MQTT 3.1.1（TLS、用户名密码、QoS 0–2、retain、多个订阅）、WebSocket。串口和各个网络连接可以同时连着，各自一份日志，也可以按时间合并成一份看，状态栏显示哪些在线。日志、终端、快捷命令等功能在网络连接下全部可用。
- **低功耗蓝牙（BLE）**：扫描附近设备（带信号格），连接后列出服务和特征，订阅通知、读取，在命令栏直接写入。Nordic UART、HM-10（FFE0）这类蓝牙串口模块自动识别写入特征。
- **HTTP 接口调试**：类似 Postman 的 **API** 标签，调开发板上的 REST 接口：各种方法、参数、请求头、JSON / 表单 / 文本请求体、Bearer / Basic 认证、`{{变量}}`、收藏和历史、彩色 JSON 响应，支持 cURL 导入导出（浏览器里「复制为 cURL」直接粘到地址栏）。
- **快捷命令**：多组一键发送的命令（文本或 HEX），每条可单独设换行符，`Alt+1…9` 快捷发送，可按间隔依次发送整组。
- **两个页面并排看**：串口日志放在代码旁边、仿真串口放在电路旁边、比较放在文件旁边——中键点或把任意页面、工具拖到右边或下方即可。第一次用得上时会自动出现（烧录后、电路通电、日志里出现 HardFault），来回切页时提示放在一起，按工程记住布局，还能弹出到第二块屏幕。
- **代码浏览器**：在日志旁打开固件工程，文件树、多标签、12 类语言语法高亮、按文件名快速打开、全工程搜索；C / C++ 支持跳转到定义、查找引用、宏展开并计算出数值。
- **完整的代码编辑器**：多光标、列选择、代码折叠、括号匹配、粘性滚动、行操作，以及 Source Insight 式的上下文窗口、调用关系、书签和语义着色。装了 clangd 且工程有 `compile_commands.json` 时自动使用 clangd。
- **代码生成**：F8 多色高亮，`#if 0` / 未定义 `#ifdef` 的代码变灰，`#ifdef` 和 `#if 0` 一键包裹，按类型自动选格式的变量打印，Doxygen 和文件头说明，16 个可自定义宏动作，按键录制，导航历史，可切换 VS Code / Source Insight / Keil 快捷键方案。代码、Git、终端、文件服务器共用一个工程文件夹。
- **内置终端、查找替换、分屏**：底部终端面板（PowerShell、cmd、Git Bash、bash 等），一键编译 / 烧录任务按钮、编译错误「问题」列表、按命令分块复制和跳转、查找、标签着色和并排，`文件:行号` 可点击跳转；文件内和全工程的正则查找替换带预览；拆分编辑器、Git 行标记和行内 Blame、外部修改自动重载、崩溃恢复、便携模式。
- **固件工具**：Map / ELF / HEX / BIN 查看器（内存占用条、字节解读）、文本和二进制文件比较、带字段提取的位域计算器、数字悬停、代码片段、TODO 列表、Kconfig / 设备树 / 链接脚本高亮、可选 Vim 键位。
- **调试工具**：日志数值实时波形图、过滤 / 高亮规则、二进制协议分帧和校验检查、自动应答、[Rhai](https://rhai.rs) 测试脚本、Modbus RTU / TCP 主站、XMODEM / YMODEM 发送文件、发送时自动加 CRC、AT 指令助手、多行发送、按原时序回放日志、统计、标记和书签。
- **串口扩展**：DTR / RTS / Break 控制和 CTS / DSR / RI / CD 实时状态，一键复位（含 ESP32 下载模式时序），串口共享到网络、虚拟串口（烧录工具和别的串口助手可同时使用，不占用 COM 口；Windows 一键下载安装 com0com 驱动），双口监听，端口别名，配置方案，多窗口。
- **电路仿真和接线记录**：在面包板上搭电路、通电看结果，真实求解器，接错会冒烟；万用表、示波器、逻辑分析仪；单片机引脚不写代码也能动（还能生成 Arduino / STM32 HAL / ESP-IDF / 51 代码）；AT 模块能连真实网络；72 个带讲解的示例。接线表配实物照片和编号标记，存在工程的 `.qccom/wiring/` 里。
- **比较与合并**（`Ctrl+Shift+M`）：内置 Beyond Compare 式比较。文本比较带行内差异，两边都能直接改、一键把一段推到另一边；文件夹比较和同步；十六进制比较能自动对齐插入的字节，HEX 按地址比较；Git 冲突三方合并；CSV 和 map 符号表比较；HTML 报告。到处都能右键比较，包括系统文件管理器（「用 QCCOM 打开」「用 QCCOM 比较」）。
- **外部修改和本地历史**：AI 工具、Keil 等别的程序改了工程文件会列出来，逐块保留或还原；每个文件自动留版本，一键打存档点、整个工程回到任意存档点，只存本机、不动 `.git`。
- **查看器**：图片（GIF 播放、逐帧、转 C 数组、两图对比）、原始像素（RGB565 显存）、Markdown 预览（分屏同步滚动）、JSON / YAML / TOML 树、CSV 表格和曲线。
- **寄存器预览**：从 Keil / IAR / CubeMX / ESP-IDF 等工程配置自动找到芯片的 SVD，悬停寄存器和数值按位域解码，寄存器面板和转储对比。
- **Git**：本地 / 远程分支和标签、所有分支的提交图、提交详情、带行号的差异对比；提交、推送、按文件或代码块暂存、提交前检查、撤销上次提交、stash、打标签、子模块，每次提交记录固件体积，状态栏显示当前分支。
- **SSH 和 Telnet**：远程登录开发板和网关，多个会话同时在线；用系统自带的 OpenSSH（密钥、agent、`~/.ssh/config` 都能用），把文件拖到终端上就能上传。
- **工具箱**（`Ctrl+Shift+B`）：所有通信调试工具（TCP、UDP、MQTT、BLE、WebSocket、SSH / Telnet、HTTP 接口、Modbus 从站）排在最前，另有 TFTP / HTTP 文件服务器、本地 MQTT 服务器、Syslog 接收、局域网设备发现、Ping 和端口检测、子网计算、数据转换、校验计算（含自定义 CRC）、时间戳、正则测试、字模 / 图片取模、颜色转换、ASCII 表、UART 波特率计算、SLCAN CAN 调试和 USB HID。可搜索（支持拼音首字母）、可收藏、记住最近使用，每个工具都能从命令面板直接打开；校验计算也能算文件的 MD5 / SHA1 / SHA256。
- **嵌入式工具**：固件转换 / 合并（HEX、BIN、S-record、ELF）、带 CRC 的升级包头、两次编译的体积对比（支持 GCC、Keil、LLVM 的 map）、最坏栈深度分析、中断向量表（标出没实现的中断）、SVD 寄存器查看（编辑器里悬停也能看）、Keil / IAR 工程导入生成 `compile_commands.json`、CubeMX 引脚表、电阻 / 分压 / NTC / PWM / I2C 上拉计算、C 结构体解析、CBOR / MessagePack / Protobuf 解码、VCD 波形查看和二维码。
- **录制与导出**：保存或复制日志；也可以把之后的每一行实时写入文件（可按大小或时长分卷），适合长时间挂机抓日志。
- **自动更新**：从你自己的服务器检查新版本，下载后做 SHA-256 校验再替换。格式见 [docs/UPDATE_SERVER.md](docs/UPDATE_SERVER.md)。
- **深色 / 浅色主题**（或跟随系统，Windows 标题栏颜色同步）、可选背景图片、可搜索的**命令面板**（`Ctrl+Shift+P`）、设置中心，以及**完全可自定义的快捷键**（支持 Ctrl+K Ctrl+S 这类序列键）。
- **10 种界面语言**，按系统语言自动选择：English、简体中文、繁體中文、日本語、한국어、Deutsch、Français、Español、Русский、Português。

| 调试工具 | 固件工具 |
|---|---|
| ![波形图、规则和自动应答](docs/screenshot-debug-tools.png) | ![Map 查看器和位域计算器](docs/screenshot-embedded.png) |

| 代码浏览器 | Git 历史 |
|---|---|
| ![代码浏览器](docs/screenshot-code.png) | ![Git 查看器](docs/screenshot-git.png) |

| 终端模式 | 浅色主题 |
|---|---|
| ![终端](docs/screenshot-terminal.png) | ![浅色主题](docs/screenshot-light.png) |

| HTTP 接口 | 低功耗蓝牙 |
|---|---|
| ![API 标签](docs/screenshot-api.png) | ![BLE 标签](docs/screenshot-ble.png) |

| 工具箱：CAN 调试 | 比较 |
|---|---|
| ![工具箱里的 CAN 调试](docs/screenshot-toolbox.png) | ![文本比较](docs/screenshot-compare.png) |

| 体积对比 | VCD 波形 |
|---|---|
| ![两次编译的体积对比](docs/screenshot-sizediff.png) | ![VCD 波形查看](docs/screenshot-vcd.png) |

## 动图演示

| 自动波特率、彩色日志、搜索 | 日志数值实时曲线 |
|---|---|
| ![连接](docs/gif/demo-connect.gif) | ![曲线](docs/gif/demo-plot.gif) |
| **终端模式** | **宏展开和跳转定义** |
| ![终端](docs/gif/demo-terminal.gif) | ![代码](docs/gif/demo-code.gif) |
| **HTTP 接口调试** | **工具箱** |
| ![API](docs/gif/demo-api.gif) | ![工具箱](docs/gif/demo-toolbox.gif) |
| **串口日志放在代码旁边** | |
| ![副页面](docs/gif/demo-side.gif) | |
| **编辑器补全和函数跳转** | **电压和电流示波器** |
| ![编辑器补全和函数跳转](docs/gif/demo-editor-navigation.gif) | ![电压和电流示波器](docs/gif/demo-circuit-instruments.gif) |

[中文使用手册](docs/USER_GUIDE.zh-CN.md) · [图文操作指南（Word）](docs/QCCOM-操作指南.docx)

## 下载

到 [Releases 页面](https://github.com/zenghi/QCCOM/releases) 下载最新版。

| 系统 | 文件 | 说明 |
|---|---|---|
| Windows 10 / 11 x64 | `qccom-windows-x64.exe` | 双击运行。如果系统没有自带，需要装 USB 转串口驱动（CH340、CP210x、FTDI 等）。 |
| Linux x64 | `qccom-linux-x64` | `chmod +x qccom-linux-x64 && ./qccom-linux-x64`。普通用户打开串口需要加入 `dialout` 组（Arch 是 `uucp`）：`sudo usermod -aG dialout $USER`，重新登录后生效。 |

每个版本都附带 `SHA256SUMS.txt`，可以用来校验下载的文件。

**代码签名**。Windows 正式发布要求通过 SignPath 签名并验证 Authenticode。新构建在积累信誉期间仍可能触发 SmartScreen 提示。详见[如何验证 Windows 下载](CODE_SIGNING.md#验证-windows-下载)和[隐私政策](https://www.zenghi.com/privacy/)。

## 快速上手

1. 在左侧选择端口。
2. 点 **连接** 或按 **F5**。不知道波特率就在设备输出日志时点 **⚡ 自动**。
3. 看日志。在底部输入命令，按 **回车** 发送。

想像 PuTTY、minicom 那样直接敲命令，就点顶栏的 **终端** 或按 **Ctrl+\`**。要用 TCP、UDP、MQTT 或 BLE，点设备列表上方的对应标签；**API** 标签打开 HTTP 请求编辑器。

完整说明见 [使用手册](docs/USER_GUIDE.zh-CN.md)。

### 在内置终端中编译和烧录 MCU

在代码页打开工程根目录，再打开底部终端。QCCOM 会根据工程文件查找已安装的工具链。 **MCU 工具链** 可调整 SDK、构建目录、开发板等工程设置。

ESP32 的 ESP-IDF 流程已有实板验证。其它工具链在选择列表和终端工具栏上标为「未测试」。

在 **调试 → 设备** 选择串口，或用终端工具栏上的串口下拉框切换同一设备。烧录命令会读取当前设备，**串口监视** 在调试页打开该设备，并使用调试会话的波特率。终端会等已连接的串口释放后才开始烧录。

ESP32 工程可把构建目录设为 `build/field-itwt-qq7-tcp-5s-8ms`，在设备列表选中 `COM30` 后点 **自定义命令 → 烧录应用并监视**。也可以在 QCCOM 的 ESP-IDF 终端直接执行

```powershell
idf.py -B build/field-itwt-qq7-tcp-5s-8ms -p COM30 app-flash monitor
```

直接输入 `make` 会在 ESP-IDF CMake 工程根目录执行编译。`make flash` 和 `make app-flash` 使用选中的设备，`make monitor` 打开调试串口。GNU Make 工程可使用自己的 Makefile。

| 工程 | 终端操作 |
| --- | --- |
| ESP-IDF（ESP32、S2、S3、C3、C6、H2 等） | 从 `IDF_PATH`、Espressif 安装管理器或常见安装目录加载 SDK 和 Python 环境。支持编译、整包烧录、仅烧录应用、清理、`menuconfig` 和固件大小。具体芯片取决于 SDK 版本。 |
| STM32CubeMX / CubeIDE | 用 Makefile、CMake 或 CubeIDE 的命令行构建。选择固件后用 STM32CubeProgrammer 烧录，默认 SWD。选择「设备串口」可用串口烧录。`.bin` 需要填写起始地址。 |
| 沁恒微 WCH / MounRiver Studio | 自动查找 MRS Community 和 MRS 2/3 的 GNU Make、RISC-V 工具链及 WCHISPStudio 官方烧录工具。进入含 `.wvproj` 的工程目录后可输入 `make -j8`。支持工程的 Makefile、CMake 和 Community 版的 Eclipse CDT 首次构建。烧录可用 `wch-flash`、终端按钮或工程自己的目标。 |
| PlatformIO | 用 `pio run` 编译、上传和清理。使用 ST-Link 或网络上传等工程专用方式时，可勾选「烧录接口由 platformio.ini 指定」。 |
| Zephyr | 用 `west` 编译和烧录。填写开发板名称，烧录使用该板的 runner。可覆盖 Nordic nRF、NXP、STM32 等 Zephyr 支持的板卡。 |
| Raspberry Pi Pico SDK | 用 CMake 编译 RP2040 / RP2350 工程，选择 `.uf2` 后用 `picotool` 烧录。 |
| Arduino CLI | 填写 FQBN，通过已安装的 board core 编译并使用当前设备串口上传。 |
| 通用 Make / CMake | 编译、清理以及工程定义的烧录目标，适用于已配置好编译器及烧录程序的工程。 |

旧版 `.cproject` 工程首次构建可调用已安装的 MounRiver Studio Community 生成 Makefile。MRS 2/3 可使用已有的 Makefile，或使用 IDE 导出的 CMake 工程。从 SDK 上层目录打开时，自动定位唯一匹配的子工程。多个候选无法区分时，在 **MCU 工具链** 里选择子工程。安装在自定义位置时，可在 **MCU 工具链** 中选择 MounRiver 的安装根目录。

沁恒烧录默认优先使用 WCHISPStudio 的芯片系列命令行程序，例如 CH582 使用 `WCHISPTool_CH57x-59x.exe`。在 WCHISPStudio 选择芯片和接口，用 **文件 → 保存界面配置** 导出 INI，再在 QCCOM 的 **MCU 工具链** 中选择它。串口烧录跟随 **调试 → 设备** 中选择的 COM。CH57x–59x 的自动模式会读取 INI 里的串口或 USB 设置，也可在 **烧录接口** 手动选择。独立的 WCHISPTool_CMD 在 USB 模式下还需要 USB 设备标识。让芯片进入 BOOT 后点 **烧录** 或运行 `wch-flash`。 [WCHISPStudio 下载](https://www.wch.cn/downloads/WCHISPTool_Setup_exe.html) [WCHISPTool_CMD 下载](https://www.wch.cn/downloads/WCHISPTool_CMD_ZIP.html)


通过 **自定义命令 → 编辑任务** 添加命令，例如 `idf.py size-components`。每条保存的命令都会在终端工具栏生成按钮，右键可编辑。Keil、IAR 等其他工具可从 **手动导入环境** 配置路径，再添加工程的编译和烧录命令。

## 从源码编译

需要较新的 Rust 稳定版（用 `rustup` 安装，1.85 或更高）。

```bash
git clone https://github.com/zenghi/QCCOM.git
cd QCCOM

# 仅 Linux：枚举串口和蓝牙需要的系统头文件
sudo apt install pkg-config libudev-dev libdbus-1-dev   # Debian / Ubuntu
# sudo dnf install pkgconf-pkg-config systemd-devel dbus-devel   # Fedora
# sudo pacman -S pkgconf systemd-libs                 # Arch

cargo build --release
# 产物：target/release/qccom（Windows 上是 qccom.exe）

cargo test
```

在 Linux 上交叉编译 Windows 版、发布配置、CI 等细节见 [docs/BUILDING.md](docs/BUILDING.md)。

## 参与贡献

欢迎提交问题、功能建议、翻译和 PR，请先看 [CONTRIBUTING.md](CONTRIBUTING.md)。新增或修正一种界面语言只需要改一个 TSV 文件。

## 许可证与署名

QCCOM 使用 [Apache License 2.0](LICENSE) 开源。Copyright © 2026 [zenghi](https://github.com/zenghi)。

你可以自由使用、修改、再分发，包括商业用途。但再分发 QCCOM 或基于它修改的作品时，许可证要求你：

- 附带 [LICENSE](LICENSE) 副本；
- 附带 [NOTICE](NOTICE) 文件，其中包含署名 **“基于 zenghi 的 QCCOM 开发 – https://github.com/zenghi/QCCOM”**；
- 保留原有的版权和署名声明，并在修改过的文件中注明修改。

请不要删除程序“关于”窗口里显示的作者和仓库信息。

代码签名政策：[CODE_SIGNING.md](CODE_SIGNING.md)。

内置字体：Inter 和 JetBrains Mono，均为 SIL Open Font License 1.1（见 [assets/licenses](assets/licenses)）。
