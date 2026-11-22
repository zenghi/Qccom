use super::*;
use std::io::{Seek, SeekFrom};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(in crate::shell::mcu) struct EspIdf {
    pub python: PathBuf,
    pub script: PathBuf,
    #[cfg(unix)]
    pub shims: Option<tempfile::TempDir>,
}

pub(super) struct Activated {
    pub root: PathBuf,
    pub idf: EspIdf,
    pub env: BTreeMap<String, OsString>,
    pub paths: Vec<PathBuf>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Eim {
    idf_installed: Vec<Installation>,
    idf_selected_id: String,
    git_path: PathBuf,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Installation {
    id: String,
    path: PathBuf,
    python: PathBuf,
    idf_tools_path: PathBuf,
    status: String,
}

fn valid_sdk(path: &Path) -> bool {
    path.is_absolute() && path.join("tools/idf.py").is_file() && path.join("tools/idf_tools.py").is_file()
}

pub(super) fn activate(host: &Host, sdk: Option<&Path>) -> Result<Activated, String> {
    let mut bases = Vec::new();
    if let Some(tools) = host.var("IDF_TOOLS_PATH") {
        bases.push(tools);
    }
    if cfg!(windows) {
        if let Some(dir) = host.var("LOCALAPPDATA") {
            bases.push(dir.join("Espressif"));
        }
        bases.push(
            host.var("SystemRoot")
                .and_then(|p| p.parent().map(Path::to_owned))
                .unwrap_or_else(|| PathBuf::from(r"C:\"))
                .join("Espressif"),
        );
        bases.extend(host.program_files.iter().map(|p| p.join("Espressif")));
    } else {
        bases.push(PathBuf::from("/opt/esp"));
    }
    if let Some(home) = &host.home {
        bases.extend([home.join(".espressif"), home.join("esp"), home.join("Espressif")]);
    }
    let mut eim = Vec::new();
    let mut git = Vec::new();
    for base in &bases {
        for config in [base.join("eim_idf.json"), base.join("tools/eim_idf.json")] {
            let Ok(mut data) = serde_json::from_str::<Eim>(&read_prefix(&config)) else { continue };
            if let Some(dir) = data.git_path.parent().filter(|p| data.git_path.is_absolute() && p.is_dir()) {
                git.push(dir.to_owned());
            }
            data.idf_installed
                .sort_by_key(|i| (i.id != data.idf_selected_id, std::cmp::Reverse(numbers(&i.path.to_string_lossy()))));
            eim.extend(data.idf_installed.into_iter().filter(|i| i.status.is_empty() || i.status == "finished"));
        }
    }
    let mut candidates = Vec::new();
    if let Some(dir) = sdk {
        candidates.push(dir.to_owned());
    } else {
        candidates.extend(host.var("IDF_PATH"));
        candidates.extend(eim.iter().map(|i| i.path.clone()));
        candidates.extend(host.path.iter().filter(|p| p.join("idf.py").is_file()).filter_map(|p| p.parent().map(Path::to_owned)));
        for base in &bases {
            candidates.push(base.join("esp-idf"));
            candidates.push(base.clone());
            for frameworks in [base.join("frameworks"), base.clone()] {
                for version in child_dirs(&frameworks, "") {
                    candidates.extend([version.join("esp-idf"), version]);
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut candidates = candidates.into_iter().filter(|path| valid_sdk(path) && seen.insert(path.clone()));
    let root = candidates.next().ok_or_else(|| {
        if let Some(sdk) = sdk {
            tf("目录 {} 缺少 ESP-IDF 工具，请选择包含 tools/idf.py 的 SDK 根目录。", &[&sdk.display()])
        } else {
            t("未发现 ESP-IDF。请安装 Espressif SDK 和工具，或选择 ESP-IDF 根目录。").into()
        }
    })?;
    let mut error = None;
    for root in std::iter::once(root).chain(candidates) {
        let installation = eim.iter().find(|i| i.path == root);
        match activate_sdk(host, root, installation, &bases, &git) {
            Ok(sdk) => return Ok(sdk),
            Err(e) => {
                error.get_or_insert(e);
            }
        }
    }
    Err(error.expect("at least one candidate was tried"))
}

fn activate_sdk(host: &Host, root: PathBuf, installation: Option<&Installation>, bases: &[PathBuf], git: &[PathBuf]) -> Result<Activated, String> {
    let mut stores = Vec::new();
    stores.extend(installation.map(|i| i.idf_tools_path.clone()).filter(|p| p.is_absolute() && p.is_dir()));
    if let Some(dir) = host.var("IDF_TOOLS_PATH") {
        stores.push(dir);
    }
    stores.extend(
        root.ancestors()
            .take(4)
            .filter(|p| p.join("python_env").is_dir() || p.join("tools/eim_idf.json").is_file())
            .map(Path::to_owned),
    );
    stores.extend(bases.iter().cloned());
    let version_file = read_prefix(&root.join("tools/cmake/version.cmake"));
    let version = |key: &str| version_file.lines().find(|line| line.contains(key)).and_then(|line| numbers(line).first().copied());
    let prefix = match (version("IDF_VERSION_MAJOR"), version("IDF_VERSION_MINOR")) {
        (Some(major), Some(minor)) => format!("idf{major}.{minor}_"),
        _ => String::from("idf"),
    };
    let python_names: &[&str] = if cfg!(windows) { &["python"] } else { &["python", "python3"] };
    let (tools, discovered_venv) = stores
        .iter()
        .find_map(|store| {
            let venv = child_dirs(&store.join("python_env"), &prefix)
                .into_iter()
                .find(|dir| locate(&[python_bin(dir)], python_names).is_some())?;
            Some((store, Some(venv)))
        })
        .or_else(|| stores.iter().find(|p| p.is_dir()).map(|p| (p, None)))
        .ok_or_else(|| t("找不到 ESP-IDF 工具目录，请完成 SDK 工具安装。").to_owned())?;
    let requested_venv = host
        .var("IDF_PYTHON_ENV_PATH")
        .filter(|dir| host.var("IDF_PATH").as_deref() == Some(&root) && locate(&[python_bin(dir)], python_names).is_some());
    let venv = requested_venv.or(discovered_venv);
    let mut paths = git.to_vec();
    if cfg!(windows) {
        paths.extend(host.program_files.iter().map(|p| p.join("Git/cmd")));
        for store in std::iter::once(tools).chain(bases.iter()) {
            for tool in ["idf-exe", "idf-git", "idf-python", "git", "python"] {
                for package in child_dirs(&store.join("tools").join(tool), "") {
                    paths.extend([package.join("cmd"), package.join("bin"), package]);
                }
            }
        }
    }
    let python = venv
        .as_ref()
        .and_then(|p| locate(&[python_bin(p)], python_names))
        .or_else(|| installation.map(|i| i.python.clone()).filter(|p| p.is_absolute() && executable(p)))
        .or_else(|| locate(&paths, python_names))
        .or_else(|| locate(&host.path, python_names))
        .ok_or_else(|| t("找不到 ESP-IDF 所需的 Python，请完成 SDK 工具安装。").to_owned())?;

    let script = root.join("tools/idf.py");
    let mut proc = Command::new(&python);
    let input_path = std::env::join_paths(paths.iter().chain(&host.path)).map_err(|e| e.to_string())?;
    proc.arg(root.join("tools/idf_tools.py"))
        .args(["export", "--format", "key-value"])
        .current_dir(&root)
        .env("IDF_PATH", &root)
        .env("IDF_TOOLS_PATH", tools)
        .env("PATH", input_path)
        .env("PYTHONIOENCODING", "utf-8")
        .env_remove("IDF_PYTHON_ENV_PATH")
        .env_remove("ESP_IDF_VERSION")
        .env_remove("MSYSTEM");
    if let Some(venv) = &venv {
        proc.env("IDF_PYTHON_ENV_PATH", venv);
    }
    let output = run_export(proc).map_err(|error| tf("ESP-IDF 环境加载失败：{}", &[&error]))?;
    let mut env = exported_env(&output);
    if let Some(exported) = env.remove("PATH") {
        paths.extend(std::env::split_paths(&exported).filter(|p| p != Path::new("$PATH") && p != Path::new("%PATH%")));
    }
    let venv = env
        .get("IDF_PYTHON_ENV_PATH")
        .map(PathBuf::from)
        .or(venv)
        .ok_or_else(|| t("找不到此 ESP-IDF 版本的 Python 环境，请运行 Espressif 工具安装程序。").to_owned())?;
    let python = locate(&[python_bin(&venv)], python_names).ok_or_else(|| tf("ESP-IDF Python 环境无效：{}", &[&venv.display()]))?;
    paths.insert(0, python_bin(&venv));
    paths.push(root.join("tools"));
    for component in ["espcoredump", "partition_table", "app_update"] {
        paths.push(root.join("components").join(component));
    }
    env.insert("IDF_PATH".into(), root.as_os_str().into());
    env.insert("IDF_TOOLS_PATH".into(), tools.as_os_str().into());
    env.insert("IDF_PYTHON_ENV_PATH".into(), venv.into_os_string());

    #[cfg(unix)]
    let shims = make_shims().map_err(|e| tf("无法创建 ESP-IDF 终端命令：{}", &[&e]))?;
    Ok(Activated {
        root,
        env,
        paths,
        idf: EspIdf {
            python,
            script,
            #[cfg(unix)]
            shims: Some(shims),
        },
    })
}

fn exported_env(stdout: &str) -> BTreeMap<String, OsString> {
    stdout
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            (!key.is_empty() && !key.as_bytes()[0].is_ascii_digit() && key.bytes().all(|c| c == b'_' || c.is_ascii_alphanumeric()) && !value.contains('\0')).then(|| (key.to_owned(), value.into()))
        })
        .collect()
}

fn run_export(mut proc: Command) -> Result<String, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        proc.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut out = tempfile::tempfile().map_err(|e| e.to_string())?;
    let err = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut child = proc
        .stdin(Stdio::null())
        .stdout(out.try_clone().map_err(|e| e.to_string())?)
        .stderr(err.try_clone().map_err(|e| e.to_string())?)
        .spawn()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if start.elapsed() >= Duration::from_secs(45) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(t("等待 ESP-IDF 工具超时").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if status.success() {
        out.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut text = String::new();
        out.take(1024 * 1024).read_to_string(&mut text).map_err(|e| e.to_string())?;
        Ok(text)
    } else {
        let mut details = Vec::new();
        for mut file in [err, out] {
            file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
            file.take(64 * 1024).read_to_end(&mut details).map_err(|e| e.to_string())?;
        }
        let details = String::from_utf8_lossy(&details);
        let tail: Vec<_> = details.lines().rev().filter(|line| !line.trim().is_empty()).take(4).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        if tail.is_empty() {
            Err(status.to_string())
        } else {
            Err(tail)
        }
    }
}

#[cfg(unix)]
fn make_shims() -> std::io::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir()?;
    for (name, source) in [
        ("idf.py", "#!/bin/sh\nexec \"$QCCOM_IDF_PYTHON\" \"$QCCOM_IDF_SCRIPT\" \"$@\"\n"),
        (
            "make",
            r#"#!/bin/sh
target=${1-build}
if [ "$#" -le 1 ] && [ . -ef "$QCCOM_MCU_PROJECT" ] &&
   [ ! -f Makefile ] && [ ! -f makefile ] && [ ! -f GNUmakefile ]; then
    case "$target" in
        all) target=build ;;
    esac
    case "$target" in
        build|clean)
            exec "$QCCOM_IDF_PYTHON" "$QCCOM_IDF_SCRIPT" -B "$QCCOM_MCU_BUILD_DIR" "$target" ;;
    esac
fi
if [ -n "$QCCOM_REAL_MAKE" ]; then
    exec "$QCCOM_REAL_MAKE" "$@"
fi
echo 'GNU Make was not found.' >&2
exit 127
"#,
        ),
    ] {
        let path = dir.path().join(name);
        fs::write(&path, source)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn export_loads_versioned_python_and_shims_preserve_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Firmware lab");
        let sdk = dir.path().join("esp-idf-5.5");
        let python = dir.path().join("python_env/idf5.5_py3.12_env/bin/python");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(sdk.join("tools/cmake")).unwrap();
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        fs::write(root.join("CMakeLists.txt"), "include($ENV{IDF_PATH}/tools/cmake/project.cmake)").unwrap();
        fs::write(sdk.join("tools/cmake/version.cmake"), "set(IDF_VERSION_MAJOR 5)\nset(IDF_VERSION_MINOR 5)").unwrap();
        fs::write(sdk.join("tools/idf.py"), "").unwrap();
        fs::write(sdk.join("tools/idf_tools.py"), "").unwrap();
        fs::write(
            &python,
            "#!/bin/sh\nif [ \"$2\" = export ]; then\n  printf 'TEST_EXPORT=%s\\n' 'a=b c'\nelse\n  printf '<%s>\\n' \"$@\"\nfi\n",
        )
        .unwrap();
        fs::set_permissions(&python, fs::Permissions::from_mode(0o700)).unwrap();

        let session = Session::discover(
            root.clone(),
            Options {
                sdk,
                build_dir: "build/field TCP".into(),
                ..Default::default()
            },
        );
        assert_eq!(session.profile, Profile::EspIdf);
        assert!(session.issue.is_none(), "{:?}", session.issue);
        let mut builder = CommandBuilder::new("/bin/sh");
        session.apply(&mut builder, Syntax::Posix);
        assert_eq!(builder.get_env("TEST_EXPORT").unwrap(), "a=b c");
        let mut cmd = Command::new("/bin/sh");
        cmd.current_dir(&root).args(["-c", "idf.py -B 'test dir' -p COM30 app-flash monitor && make"]);
        for key in ["PATH", "QCCOM_IDF_PYTHON", "QCCOM_IDF_SCRIPT", "QCCOM_MCU_PROJECT", "QCCOM_MCU_BUILD_DIR"] {
            cmd.env(key, builder.get_env(key).unwrap());
        }
        let output = cmd.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("<-B>\n<test dir>\n<-p>\n<COM30>\n<app-flash>\n<monitor>\n"), "{stdout}");
        assert!(stdout.contains("<-B>\n<build/field TCP>\n<build>\n"), "{stdout}");

        let plan = session.command(Syntax::PowerShell, Action::AppFlashMonitor, "COM30").unwrap();
        assert!(plan.ends_with(" -B 'build/field TCP' -p COM30 app-flash } finally { Pop-Location } }"), "{plan}");
        let shortcut = session.shortcut("idf.py -B 'other build' -p COM30 app-flash monitor").unwrap();
        assert!(shortcut.action.monitor_after());
        assert!(session.shortcut_command(Syntax::PowerShell, &shortcut, "COM31").is_err());
        let task = session.shortcut_command(Syntax::PowerShell, &shortcut, "COM30").unwrap().unwrap();
        assert!(task.cmd.ends_with(" -B 'other build' -p COM30 app-flash } finally { Pop-Location } }"), "{}", task.cmd);
        let make = session.shortcut("make flash").unwrap();
        assert!(session.shortcut_command(Syntax::Posix, &make, "COM31").unwrap().unwrap().cmd.contains("-p COM31 flash"));
        assert!(session.shortcut("idf.py monitor && echo done").is_none());
    }
}
