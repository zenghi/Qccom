use super::*;

impl App {
    pub(in crate::app) fn ai_ui(&mut self, ctx: &egui::Context) {
        if !self.ai.dialog && self.ai.bridge.is_none() {
            return;
        }
        for _ in 0..4 {
            let Some(call) = self.ai.bridge.as_ref().and_then(|b| b.calls.try_recv().ok()) else { break };
            let slot = self.ai.slot;
            let result = if Instant::now() > call.deadline {
                Err("Request expired".into())
            } else if slot >= self.sessions.len() || slot != self.session_slot && self.sessions[slot].is_none() {
                Err("Shared session closed".into())
            } else {
                self.with_session(slot, |app| {
                    if app.device_key() != app.ai.key || app.ai.origin != Some(app.plot.lab.t0) || !app.plot.lab.active {
                        return Err("Device or project changed; reconnect MCP".into());
                    }
                    app.ai_call(&call.name, &call.args)
                })
            };
            self.ai.last = format!("{} · {}", call.name, result.as_ref().err().map(String::as_str).unwrap_or("OK"));
            let _ = call.reply.try_send(result);
        }
        if !self.ai.dialog {
            return;
        }
        let slot = self.ai.slot;
        if slot != self.session_slot && self.sessions.get(slot).is_none_or(|s| s.is_none()) {
            self.ai.bridge = None;
            self.ai.dialog = false;
            self.toast(t("共享会话已关闭"), WARN());
            return;
        }
        self.with_session(slot, |app| app.ai_dialog(ctx));
    }

    fn ai_dialog(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new(t("AI 连接 · MCP")).collapsible(false).open(&mut open).show(ctx, |ui| {
            if self.ai.bridge.is_none() {
                ui.label(t("共享当前调试会话"));
                ui.monospace(self.device_key());
                if ui.button(t("启用只读连接")).clicked() {
                    match crate::mcp::Bridge::open(ctx.clone()) {
                        Ok(b) => {
                            self.ai.slot = self.session_slot;
                            self.ai.key = self.device_key();
                            self.ai.device = false;
                            self.ai.build = false;
                            self.ai.circuit = false;
                            self.ai.last.clear();
                            self.ai.bridge = Some(b);
                            let shown = self.plot.lab.visible;
                            self.open_lab();
                            self.ai.origin = Some(self.plot.lab.t0);
                            self.plot.lab.visible = shown;
                        }
                        Err(e) => self.toast(e.to_string(), ERR()),
                    }
                }
            } else {
                ui.monospace(&self.ai.key);
                ui.label("MCP · stdio");
                if ui.button(t("复制客户端配置")).clicked() {
                    ctx.copy_text(serde_json::to_string_pretty(&self.ai.bridge.as_ref().unwrap().config()).unwrap());
                }
                if ui.button(t("复制 Codex TOML 配置")).clicked() {
                    ctx.copy_text(self.ai.bridge.as_ref().unwrap().codex_config());
                }
                ui.small(t("将配置加入客户端并重连，先调用 session_info 查看当前会话和权限。"));
                ui.collapsing(t("会话权限"), |ui| {
                    ui.checkbox(&mut self.ai.device, t("允许录制、发送已保存指令和调参"));
                    ui.checkbox(&mut self.ai.circuit, t("共享当前仿真电路（只读）"));
                    ui.checkbox(&mut self.ai.build, t("允许编译和烧录"));
                });
                if !self.ai.last.is_empty() {
                    ui.small(&self.ai.last);
                }
                if ui.button(t("断开共享")).clicked() {
                    self.ai.bridge = None;
                }
            }
        });
        self.ai.dialog = open;
    }
    pub(super) fn ai_call(&mut self, name: &str, args: &Value) -> crate::mcp::Answer {
        let lab = &mut self.plot.lab;
        let c = lab.data();
        let from = args["from"].as_f64().unwrap_or(-f64::MAX);
        let to = args["to"].as_f64().unwrap_or(f64::MAX);
        if from > to {
            return Err("from exceeds to".into());
        }
        match name {
            "session_info" => Ok(
                json!({"permissions":{"circuit":self.ai.circuit,"device":self.ai.device,"build":self.ai.build},"project":self.shell_cwd(),"port":self.s.port,"connected":self.can_send(),"toolchain":self.shells.instrument_context(),"firmware":self.plot.lab.data().firmware,"parameters":self.plot.lab.data().parameters,"protocol":self.plot.lab.cfg.protocol,"channels":self.plot.lab.data().channels.iter().map(|c|&c.name).collect::<Vec<_>>(),"commands":self.s.groups.iter().flat_map(|g|&g.cmds).filter(|c|c.target.is_none()).map(|c|&c.name).take(100).collect::<Vec<_>>(),"parameter":{"template":self.plot.lab.cfg.template,"min":self.plot.lab.cfg.low,"max":self.plot.lab.cfg.high},"recording":!matches!(self.plot.lab.rec,Recording::Idle)}),
            ),
            "circuit_inspect" if self.ai.circuit => self.circ.as_ref().ok_or("Open a circuit in the QCCOM simulator first")?.mcp_overview(),
            "circuit_inspect" => Err("Enable read-only circuit sharing in the MCP session permissions".into()),
            "read_events" => {
                let limit = args["limit"].as_u64().unwrap_or(40).clamp(1, 200) as usize;
                let mut events: Vec<_> = c.events.iter().rev().filter(|e| e.t >= from && e.t <= to).take(limit).collect();
                events.reverse();
                Ok(json!(events))
            }
            "read_signal" | "compare_signal" => {
                let i = args["channel"].as_u64().unwrap_or(1).checked_sub(1).and_then(|v| usize::try_from(v).ok()).ok_or("Invalid channel")?;
                let ch = c.channels.get(i).ok_or("Unknown channel")?;
                let (a, b) = capture::range(&ch.points, from, to);
                if a == b {
                    return Err("No samples in range".into());
                }
                let stats = |p: &VecDeque<Point>, a, b| {
                    let mut lo = f64::INFINITY;
                    let mut hi = f64::NEG_INFINITY;
                    let mut sum = 0.;
                    for &(_, v) in p.range(a..b) {
                        lo = lo.min(v);
                        hi = hi.max(v);
                        sum += v;
                    }
                    json!({"min":lo,"max":hi,"mean":sum/(b-a) as f64,"count":b-a})
                };
                let mut v = stats(&ch.points, a, b);
                v["unit"] = json!(ch.unit);
                v["name"] = json!(ch.name);
                if let Some(target) = args["target"].as_f64() {
                    let tol = args["tolerance"].as_f64().unwrap_or(0.02);
                    if let Some((over, settle)) = capture::step_metrics(&ch.points, from, to, target, tol) {
                        v["overshoot_percent"] = json!(over);
                        v["settling_seconds"] = json!(settle);
                    }
                }
                if name == "compare_signal" {
                    let other = lab
                        .reference
                        .as_deref()
                        .and_then(|r| r.channels.iter().find(|x| x.name == ch.name))
                        .ok_or("No matching reference channel")?;
                    let shift = other.points.front().map_or(0., |p| p.0) - ch.points.front().map_or(0., |p| p.0);
                    let (l, r) = capture::range(&other.points, from + shift, to + shift);
                    if l == r {
                        return Err("No reference samples".into());
                    }
                    v["reference"] = stats(&other.points, l, r);
                } else {
                    v["samples"] = json!(capture::envelope(&ch.points, ch.points[a].0, ch.points[b - 1].0 + 1e-9, 64));
                    if let Some(hz) = args["fft_hz"].as_f64() {
                        let subset = ch.points.range(a.max(b.saturating_sub(8192))..b).copied().collect();
                        let f = capture::spectrum(&subset, to, hz);
                        v["fft_peak_hz"] = json!(f.iter().skip(1).max_by(|a, b| a.1.total_cmp(&b.1)).map(|p| p.0));
                    }
                }
                Ok(v)
            }
            "capture_control" if self.ai.device => {
                lab.record(args["action"].as_str().unwrap_or(""))?;
                Ok(json!(true))
            }
            "set_parameter" if self.ai.device => {
                self.send_tune(args["value"].as_f64().ok_or("Missing value")?)?;
                Ok(json!(true))
            }
            "saved_command" if self.ai.device => {
                let key = args["name"].as_str().ok_or("Missing name")?;
                if !self.can_send() {
                    return Err("Device disconnected".into());
                }
                let mut matches = self.s.groups.iter().flat_map(|g| &g.cmds).filter(|c| c.target.is_none() && c.name == key);
                let cmd = matches.next().ok_or("Unknown saved command")?;
                if matches.next().is_some() {
                    return Err("Ambiguous saved command".into());
                }
                let bytes = cmd.bytes(self.s.encoding, self.line_ending())?;
                self.send_bytes(bytes);
                self.plot.lab.event("CMD", key.into());
                Ok(json!(true))
            }
            "mcu_task" if self.ai.build => {
                let flash = match args["action"].as_str() {
                    Some("build") => false,
                    Some("flash") => true,
                    _ => return Err("Invalid action".into()),
                };
                if self.s.transport != Transport::Serial {
                    return Err("Share the MCU serial session".into());
                }
                self.shells.instrument_task(&self.ctx, &self.shell_cwd(), flash, &self.s.port)?;
                Ok(json!({"queued":true}))
            }
            "mcu_task" => Err("Enable build and flash in the MCP session permissions".into()),
            "capture_control" | "set_parameter" | "saved_command" => Err("Enable device control in the MCP session permissions".into()),
            _ => Err("Unknown QCCOM tool; call tools/list".into()),
        }
    }
}
