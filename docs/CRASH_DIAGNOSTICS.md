# QCCOM 故障诊断 / QCCOM crash diagnostics

## 简体中文

- QCCOM 尝试在 Rust panic 或前台界面单次更新持续约 20 秒无响应时，保存一条本地诊断。正常空闲、可识别的系统暂停和打开原生文件选择框不会累计为卡死。看门狗不会退出、杀死或重启 QCCOM。
- 诊断只包含故障类型、QCCOM 版本、操作系统类型和 CPU 架构。不采集 panic 消息、堆栈、日志、串口/网络数据、文件、路径或密钥。
- 下次启动同一编号的窗口可查看、丢弃或确认发布故障报告。自动发布默认关闭；在「设置 → 常规 → 自动发布故障报告」主动打开后，之后启动时可使用已经登录的匹配账号，将报告发布到现有社区的公开「求助」板块。授权仅适用于该窗口、账号和 HTTPS 服务器，不会自动注册或登录。网站可能先审核，普通社区发帖的认证仍然适用。
- 如果提交开始后无法确认服务器有没有收到，不会自动重试，避免重复发帖。是否发布不明确时请先在社区查看。没有发送的报告可以留在本机以后处理。每个窗口 24 小时内最多自动提交一次，不限制手动审核后发布。
- 这是尽力诊断，不是系统级崩溃转储。操作系统强杀、原生崩溃、未进入界面更新的卡死、磁盘不可写及整机冻结可能无记录。每个锁定的窗口编号最多保留一份待处理报告，已有记录不会被新故障覆盖。

## English

- QCCOM makes a best-effort local record of a Rust panic or a focused UI update that stops responding for about 20 seconds. Idle frames, detectable system sleep, and native file dialogs do not count as a stall. The watchdog does not kill, exit, or restart QCCOM.
- A report contains only the event type, QCCOM version, OS family, and CPU architecture. It does not collect panic messages, stack traces, logs, serial or network data, files, paths, or secrets.
- The next launch in the same window slot lets you review, discard, or confirm a public post. Automatic posting is off by default. Enabling **Settings → General → Automatically post diagnostic reports** allows later launches to submit a pending report as a public Help-board post using your matching signed-in account. Permission applies only to that window slot, account, and HTTPS server. There is no automatic registration or sign-in. The site may moderate the post, and its normal community authentication still applies.
- After a submission starts, QCCOM does not retry an unconfirmed delivery because it could produce a duplicate. Check the community if the delivery status is unknown. If a report was not sent, it can be kept locally for later. Automatic attempts are limited to once per 24 hours per window slot; you can still review and post manually.
- This does not create an OS crash dump. A forced kill, native fault, stall outside a UI update, unwritable disk, or whole-machine freeze may leave no record. Each window slot with an exclusive instance lock retains at most one unresolved report and does not overwrite it with newer incidents.
