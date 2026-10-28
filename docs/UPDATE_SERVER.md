# Self-update and the version file

[English](#english) · [简体中文](#简体中文)

## English

When it starts (and when the user clicks the download icon at the bottom of the activity bar), QCCOM downloads a small JSON **version file**. If the version in it is newer than the running one, a card offers the update. The user can update now, be reminded later, or skip that version. The download is verified, the running executable is swapped for the new one, and the app restarts.

The official builds read `https://www.zenghi.com/1.json`. Forks should change `VERSION_URL` and `DEFAULT_EXE_URL` in `src/update.rs`.

### Version file format

UTF-8 JSON (a BOM is tolerated). Only `version` is required.

```json
{
  "version": "0.5.1",
  "url": "https://www.zenghi.com/1.exe",
  "sha256": "9f2c...64 hex characters...e1",
  "size": 4012544,
  "date": "2026-11-01",
  "notes": "1. New: ...\n2. Fixed: ...",
  "force": false,
  "url_linux": "https://www.zenghi.com/qccom-linux-x64",
  "sha256_linux": "..."
}
```

| Field | Type | Meaning |
|---|---|---|
| `version` | string, **required** | The new version, e.g. `"0.5.1"`. It is compared numerically part by part (`0.10.0` > `0.9.9`), and a leading `v` is ignored. The update is offered only if it is newer than the running version. |
| `url` | string | Download link for the **Windows** `.exe`. If empty, it defaults to `https://www.zenghi.com/1.exe`. |
| `sha256` | string | SHA-256 of that `.exe` as 64 hex characters (either case). **Strongly recommended.** If it is set and does not match, the download is deleted and nothing is replaced. |
| `size` | number | File size in bytes. It is only used for the progress bar when the server does not send `Content-Length`. |
| `date` | string | Release date, shown on the card. |
| `notes` | string | Release notes, shown on the card. Use `\n` for line breaks. |
| `force` | bool | `true` hides the "Later" and "Skip this version" buttons and ignores a previously skipped version. Use it for critical fixes. |
| `url_linux` | string | Download link for the Linux binary. If empty, Linux users are told that a new version exists and get an "Open download page" button instead of an automatic download. |
| `sha256_linux` | string | SHA-256 of the Linux binary. |

### What the client checks

1. HTTPS (or HTTP) GET of the version file, with a timeout and the user agent `QCCOM/<version>`.
2. A streamed download next to the executable (or into the temp folder if that directory is read-only), with a progress bar.
3. If `Content-Length` (or `size`) is known, the downloaded length must match it.
4. The file must start with an executable header (`MZ` on Windows, `\x7fELF` on Linux). This catches servers that return an HTML error page with status 200.
5. The SHA-256 must match when one is given.
6. The running executable is renamed to `qccom.old`, the new file takes its name, and it is started with `--updated`. The `.old` file is deleted on the next start. If any step fails, the original file is restored.

### Publishing a release

```bash
# Windows build
sha256sum qccom-windows-x64.exe      # PowerShell: Get-FileHash .\qccom-windows-x64.exe -Algorithm SHA256
stat -c %s qccom-windows-x64.exe     # size in bytes (optional)
```

1. Upload the executable first (for example as `1.exe`).
2. Then upload the edited `1.json`. Doing it in this order means nobody sees a version file that points at a missing or old binary.
3. Serve both over HTTPS with `Content-Type: application/json` for the JSON. Make sure a CDN does not cache the JSON for long.

### Testing locally

```bash
mkdir upd && cd upd
cp /path/to/new/qccom.exe 1.exe
cat > 1.json <<'EOF'
{ "version": "9.9.9", "url": "http://127.0.0.1:8000/1.exe", "notes": "test" }
EOF
python3 -m http.server 8000
# in another terminal
QCCOM_UPDATE_URL=http://127.0.0.1:8000/1.json ./qccom
```

Users can turn the automatic check off by right-clicking the download icon or using the switch on the update card. A manual check is always available.

---

## 简体中文

QCCOM 启动时（以及用户点左侧活动栏底部的下载图标时）会下载一个很小的 JSON **版本文件**。如果里面的版本比当前运行的新，就弹出更新卡片，用户可以选择立即更新、稍后提醒或跳过这个版本。下载完成后会先校验，再替换正在运行的程序并自动重启。

官方版本读取的地址是 `https://www.zenghi.com/1.json`。如果你 fork 了项目，请修改 `src/update.rs` 里的 `VERSION_URL` 和 `DEFAULT_EXE_URL`。

### 版本文件格式

UTF-8 编码的 JSON（带 BOM 也可以），只有 `version` 是必填的。

```json
{
  "version": "0.5.1",
  "url": "https://www.zenghi.com/1.exe",
  "sha256": "9f2c...64 位十六进制...e1",
  "size": 4012544,
  "date": "2026-11-01",
  "notes": "1. 新增 ...\n2. 修复 ...",
  "force": false,
  "url_linux": "https://www.zenghi.com/qccom-linux-x64",
  "sha256_linux": "..."
}
```

| 字段 | 类型 | 含义 |
|---|---|---|
| `version` | 字符串，**必填** | 新版本号，如 `"0.5.1"`。逐段按数字比较（`0.10.0` 大于 `0.9.9`），开头的 `v` 会忽略。只有比当前版本新才会提示。 |
| `url` | 字符串 | **Windows** 版 exe 的下载地址。不填时默认是 `https://www.zenghi.com/1.exe`。 |
| `sha256` | 字符串 | 这个 exe 的 SHA-256，64 位十六进制，大小写都可以。**强烈建议填写**：填了而校验不一致时，会删除下载的文件，不做替换。 |
| `size` | 数字 | 文件字节数。只在服务器没有返回 `Content-Length` 时用来显示进度。 |
| `date` | 字符串 | 发布日期，显示在更新卡片上。 |
| `notes` | 字符串 | 更新说明，显示在更新卡片上，用 `\n` 换行。 |
| `force` | 布尔 | 设为 `true` 时隐藏“稍后”和“跳过此版本”按钮，并且忽略用户之前跳过的版本。适合重要修复。 |
| `url_linux` | 字符串 | Linux 版的下载地址。不填时，Linux 用户只会看到有新版的提示和“打开下载页”按钮，不会自动下载。 |
| `sha256_linux` | 字符串 | Linux 版文件的 SHA-256。 |

### 客户端会检查什么

1. 用 HTTPS（或 HTTP）GET 版本文件，有超时，User-Agent 为 `QCCOM/<版本号>`。
2. 把新文件流式下载到程序旁边（那个目录没有写权限时放到临时目录），同时显示进度。
3. 如果知道 `Content-Length`（或 `size`），下载的长度必须和它一致。
4. 文件开头必须是可执行文件头（Windows 是 `MZ`，Linux 是 `\x7fELF`）。这样可以拦住服务器返回 200 状态码但内容是 HTML 错误页的情况。
5. 填了 SHA-256 时必须一致。
6. 把正在运行的程序改名为 `qccom.old`，新文件改成原来的名字，再带 `--updated` 参数启动它；`.old` 文件在下次启动时删除。任何一步失败都会还原原来的文件。

### 发布新版本的步骤

```bash
# Windows 版
sha256sum qccom-windows-x64.exe      # PowerShell：Get-FileHash .\qccom-windows-x64.exe -Algorithm SHA256
stat -c %s qccom-windows-x64.exe     # 文件字节数（可选）
```

1. 先上传新的 exe（比如命名为 `1.exe`）。
2. 再上传改好的 `1.json`。按这个顺序，用户就不会读到一个指向不存在或旧文件的版本文件。
3. 两个文件都用 HTTPS 提供，JSON 的 `Content-Type` 设为 `application/json`。如果用了 CDN，不要让它把 JSON 缓存太久。

### 本地测试

```bash
mkdir upd && cd upd
cp /path/to/new/qccom.exe 1.exe
cat > 1.json <<'EOF'
{ "version": "9.9.9", "url": "http://127.0.0.1:8000/1.exe", "notes": "test" }
EOF
python3 -m http.server 8000
# 另开一个终端
QCCOM_UPDATE_URL=http://127.0.0.1:8000/1.json ./qccom
```

用户可以右键下载图标，或用更新卡片上的开关关闭自动检查；手动检查始终可用。

## Troubleshooting

- **"The server's HTTPS certificate does not match the domain"** (on Windows the raw text is *"The target principal name is incorrect"* or, in Chinese, *"证书中的 CN 名称与传递的值不匹配"*): the certificate served for the update host does not list that host name. Issue one that covers every name you use, e.g. `certbot --nginx -d example.com -d www.example.com`, make sure that site uses it with the full chain, and check with `openssl s_client -connect example.com:443 -servername example.com </dev/null | openssl x509 -noout -subject -ext subjectAltName`. The `url` of the download in the JSON must also be covered.
- **"The server's HTTPS certificate is invalid"**: expired, self-signed or missing intermediate certificates. Opening the JSON URL in a browser shows the same problem.
- To test against another address without rebuilding, set the environment variable `QCCOM_UPDATE_URL` to the JSON URL.
