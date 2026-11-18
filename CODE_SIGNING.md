# Code signing policy

[中文说明在下方](#代码签名政策)

The Windows release workflow uses [SignPath.io](https://about.signpath.io/) with a certificate from the [SignPath Foundation](https://signpath.org/) once the project is enrolled.

## What is signed

- Only `qccom-windows-x64.exe`, built by the public GitHub Actions workflow [`.github/workflows/build.yml`](.github/workflows/build.yml) on GitHub-hosted runners from a `v*` tag of this repository.
- Every executable that is signed is compiled from the source code in this repository. QCCOM does not embed third-party executables; the optional com0com virtual-port driver is downloaded from its own distribution only when the user asks for it and is verified by SHA-256 before it runs.
- Linux binaries are not signed.
- Builds made on a developer's machine are never submitted for signing.

## Verify a Windows download

In PowerShell, run the following from the folder containing the EXE. A signed release should show `Valid` and a certificate publisher. Compare the SHA-256 against the published `SHA256SUMS.txt` or `1.json` for the same download. SmartScreen may still warn while a new build gains reputation.

```powershell
Get-AuthenticodeSignature .\qccom-windows-x64.exe | Format-List Status, SignerCertificate
Get-FileHash .\qccom-windows-x64.exe -Algorithm SHA256
```

## Team roles

| Role | Members |
|---|---|
| Committers and reviewers | [zenghi](https://github.com/zenghi) |
| Approvers | [zenghi](https://github.com/zenghi) |

- Committers may change the source code directly. Changes proposed by anyone else (pull requests) are reviewed by a committer before they are merged.
- Every signing request is approved manually by an approver in SignPath after checking that it comes from the expected tag and workflow run.
- All team members use multi-factor authentication for GitHub and SignPath.

## Privacy

This program sends anonymous usage statistics (a random device ID, version, OS, language, usage time and page-open counts) to www.zenghi.com. Users are told on first run and can turn it off at any time in *Settings → General*. It also checks www.zenghi.com for updates and contacts the community server only when the user opens the community page. Serial data, logs, code and project files never leave the computer. Full details: [Privacy policy](https://www.zenghi.com/privacy/) ([English](https://www.zenghi.com/en/privacy/)).

## Reporting

If you find a signed file that you believe violates this policy, please open an issue or email 309966755@qq.com.

---

# 代码签名政策

项目通过审批后，Windows 发布流程使用 [SignPath.io](https://about.signpath.io/) 签名，证书由 [SignPath Foundation](https://signpath.org/) 签发。

## 签名范围

- 只签 `qccom-windows-x64.exe`：由本仓库公开的 GitHub Actions 工作流 [`.github/workflows/build.yml`](.github/workflows/build.yml) 在 GitHub 托管的构建机上、从本仓库的 `v*` 标签构建。
- 被签名的程序全部由本仓库源码编译。QCCOM 不内嵌第三方可执行文件；可选的 com0com 虚拟串口驱动只在用户要求时从其发布地址下载，并校验 SHA-256 后才运行。
- Linux 版本不签名。
- 开发者本机编译的文件不会提交签名。

## 验证 Windows 下载

在 EXE 所在文件夹运行以下 PowerShell 命令。签名版的状态应为 `Valid`，并显示签名证书。将 SHA-256 与对应下载的 `SHA256SUMS.txt` 或 `1.json` 对照。新构建在积累信誉期间仍可能触发 SmartScreen 提示。

```powershell
Get-AuthenticodeSignature .\qccom-windows-x64.exe | Format-List Status, SignerCertificate
Get-FileHash .\qccom-windows-x64.exe -Algorithm SHA256
```

## 团队角色

| 角色 | 成员 |
|---|---|
| 提交者和审核人 | [zenghi](https://github.com/zenghi) |
| 批准人 | [zenghi](https://github.com/zenghi) |

- 提交者可以直接修改源码；其他人提出的修改（Pull Request）由提交者审核后才合并。
- 每次签名请求都由批准人在 SignPath 上人工批准，批准前核对它来自预期的标签和工作流。
- 所有成员的 GitHub 和 SignPath 账号都开启了两步验证。

## 隐私

本程序会向 www.zenghi.com 发送匿名使用统计（随机设备 ID、版本、系统、语言、使用时长、页面打开次数），首次运行时会提示，可随时在「设置 → 常规」关闭。程序还会访问 www.zenghi.com 检查更新，只在用户打开社区页面时连接社区服务器。串口数据、日志、代码和工程文件不会离开电脑。详见[隐私政策](https://www.zenghi.com/privacy/)。

## 举报

如果发现违反本政策的签名文件，请提交 Issue 或发邮件到 309966755@qq.com。
