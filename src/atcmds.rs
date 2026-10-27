//! AT 指令助手：命令栏输入 AT 开头时提示常用指令（3GPP 通用、ESP-AT、蜂窝模组、蓝牙模块）。
//! 说明只有中英两种：中文界面显示中文，其余语言显示英文。

/// (指令, English, 中文)
pub const AT: &[(&str, &str, &str)] = &[
    ("AT", "Test the connection", "测试连接"),
    ("ATE0", "Echo off", "关闭回显"),
    ("ATE1", "Echo on", "打开回显"),
    ("ATI", "Product information", "产品信息"),
    ("AT&F", "Restore factory settings", "恢复出厂设置"),
    ("AT&W", "Save settings", "保存设置"),
    ("ATZ", "Reset to saved profile", "复位到保存的配置"),
    ("AT+GMR", "Firmware version", "固件版本"),
    ("AT+GMM", "Model", "型号"),
    ("AT+GMI", "Manufacturer", "厂商"),
    ("AT+GSN", "Serial number / IMEI", "序列号 / IMEI"),
    ("AT+RST", "Restart the module", "重启模块"),
    ("AT+RESTORE", "Restore factory defaults", "恢复出厂设置"),
    ("AT+UART_CUR=", "Set UART (baud,data,stop,parity,flow), not saved", "设置串口参数（不保存）"),
    ("AT+UART_DEF=", "Set UART and save", "设置串口参数并保存"),
    ("AT+SLEEP=", "Sleep mode", "休眠模式"),
    ("AT+SYSRAM?", "Free heap", "剩余内存"),
    ("AT+CWMODE=", "Wi-Fi mode: 1 station, 2 AP, 3 both", "Wi-Fi 模式：1 站点 2 热点 3 两者"),
    ("AT+CWJAP=", "Join an access point: \"ssid\",\"pwd\"", "连接热点：\"名称\",\"密码\""),
    ("AT+CWJAP?", "Current access point", "当前连接的热点"),
    ("AT+CWLAP", "List access points", "扫描热点"),
    ("AT+CWQAP", "Disconnect from the access point", "断开热点"),
    ("AT+CWSAP=", "Configure the soft AP", "设置本机热点"),
    ("AT+CIFSR", "Local IP and MAC", "本机 IP 和 MAC"),
    ("AT+CIPSTA?", "Station IP", "站点 IP"),
    ("AT+CIPSTART=", "Open TCP/UDP: \"TCP\",\"host\",port", "建立 TCP/UDP 连接"),
    ("AT+CIPSEND=", "Send data (length)", "发送数据（长度）"),
    ("AT+CIPCLOSE", "Close the connection", "关闭连接"),
    ("AT+CIPSTATUS", "Connection status", "连接状态"),
    ("AT+CIPMUX=", "Multiple connections 0/1", "多连接 0/1"),
    ("AT+CIPSERVER=", "Start / stop a TCP server", "开启 / 关闭 TCP 服务器"),
    ("AT+CIPMODE=", "Transparent mode 0/1", "透传模式 0/1"),
    ("AT+CIPDOMAIN=", "DNS lookup", "域名解析"),
    ("AT+PING=", "Ping a host", "Ping 主机"),
    ("AT+CIPSNTPCFG=", "Configure SNTP", "配置 SNTP 时间"),
    ("AT+CIPSNTPTIME?", "Current SNTP time", "当前网络时间"),
    ("AT+MQTTUSERCFG=", "MQTT user config", "MQTT 用户配置"),
    ("AT+MQTTCONN=", "MQTT connect", "MQTT 连接"),
    ("AT+MQTTPUB=", "MQTT publish", "MQTT 发布"),
    ("AT+MQTTSUB=", "MQTT subscribe", "MQTT 订阅"),
    ("AT+CPIN?", "SIM card status", "SIM 卡状态"),
    ("AT+CSQ", "Signal quality (rssi,ber)", "信号质量（rssi,ber）"),
    ("AT+CREG?", "Network registration", "网络注册状态"),
    ("AT+CEREG?", "LTE registration", "LTE 注册状态"),
    ("AT+CGREG?", "GPRS registration", "GPRS 注册状态"),
    ("AT+COPS?", "Current operator", "当前运营商"),
    ("AT+CGATT?", "Packet service attach", "数据业务附着"),
    ("AT+CGDCONT=", "Define PDP context (APN)", "设置 APN"),
    ("AT+CGACT=", "Activate PDP context", "激活 PDP"),
    ("AT+CGPADDR", "PDP address (IP)", "获取 IP"),
    ("AT+CIMI", "IMSI", "IMSI"),
    ("AT+CCID", "SIM ICCID", "SIM 卡 ICCID"),
    ("AT+CFUN=", "Phone functionality: 0 min, 1 full, 4 flight", "功能模式：0 最小 1 全功能 4 飞行"),
    ("AT+CMGF=", "SMS format: 0 PDU, 1 text", "短信格式：0 PDU 1 文本"),
    ("AT+CMGS=", "Send SMS", "发送短信"),
    ("AT+CMGL=", "List SMS", "列出短信"),
    ("ATD", "Dial", "拨号"),
    ("ATH", "Hang up", "挂断"),
    ("ATA", "Answer", "接听"),
    ("AT+CCLK?", "Clock", "时钟"),
    ("AT+CBC", "Battery / supply voltage", "电池 / 电压"),
    ("AT+QENG=", "Engineering mode (Quectel)", "工程模式（移远）"),
    ("AT+QCCID", "ICCID (Quectel)", "ICCID（移远）"),
    ("AT+QPOWD", "Power down (Quectel)", "关机（移远）"),
    ("AT+NAME", "Bluetooth name", "蓝牙名称"),
    ("AT+NAME=", "Set Bluetooth name", "设置蓝牙名称"),
    ("AT+BAUD", "Bluetooth module baud rate", "蓝牙模块波特率"),
    ("AT+ADDR?", "Bluetooth address", "蓝牙地址"),
    ("AT+ROLE=", "Role: 0 slave, 1 master", "角色：0 从机 1 主机"),
    ("AT+PIN", "Pairing PIN", "配对密码"),
    ("AT+BLEINIT=", "BLE init: 1 client, 2 server", "BLE 初始化：1 客户端 2 服务端"),
    ("AT+BLESCAN=", "BLE scan", "BLE 扫描"),
    ("AT+BLEADVSTART", "Start BLE advertising", "开始 BLE 广播"),
];

/// 输入的前缀能匹配到的指令（最多 n 条），完全相同的排在最前
pub fn matches(input: &str, n: usize) -> Vec<(&'static str, &'static str)> {
    let q = input.trim().to_ascii_uppercase();
    if q.len() < 2 || !q.starts_with("AT") {
        return vec![];
    }
    let zh = matches!(crate::i18n::current(), 0 | 2);
    let mut v: Vec<(&str, &str)> = AT
        .iter()
        .filter(|(c, ..)| c.starts_with(&q) && *c != q)
        .map(|(c, en, cn)| (*c, if zh { *cn } else { *en }))
        .collect();
    v.sort_by_key(|(c, _)| c.len());
    v.truncate(n);
    v
}

/// 完全匹配的指令的说明（输入完整指令后在提示里显示）
pub fn describe(input: &str) -> Option<&'static str> {
    let q = input.trim().to_ascii_uppercase();
    let zh = matches!(crate::i18n::current(), 0 | 2);
    let key = q.split(['=', '?']).next().unwrap_or("");
    AT.iter()
        .find(|(c, ..)| *c == q || c.trim_end_matches(['=', '?']) == key)
        .map(|(_, en, cn)| if zh { *cn } else { *en })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix() {
        let m = matches("at+cw", 5);
        assert!(m.iter().all(|(c, _)| c.starts_with("AT+CW")));
        assert!(!m.is_empty());
        assert!(matches("hello", 5).is_empty());
        assert!(describe("AT+CWJAP=\"x\",\"y\"").is_some());
    }
}
