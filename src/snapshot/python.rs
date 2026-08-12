use std::path::Path;

use serde::Deserialize;

use super::{PythonInfo, command};

#[derive(Deserialize)]
struct Probe {
    executable: String,
    version: String,
    prefix: String,
    base_prefix: String,
    torch_distribution_version: Option<String>,
    torch_version_file: Option<String>,
}

pub fn scan(cwd: &Path) -> PythonInfo {
    let virtual_env = std::env::var_os("VIRTUAL_ENV").map(Into::into);
    let conda_prefix = std::env::var_os("CONDA_PREFIX").map(Into::into);
    let mut python_candidates = command::find_executables("python3");
    python_candidates.extend(command::find_executables("python"));
    python_candidates.dedup();
    let Some(python) = python_candidates.first().cloned() else {
        return PythonInfo {
            virtual_env,
            conda_prefix,
            ..Default::default()
        };
    };
    // Reading torch/version.py avoids importing PyTorch and loading hundreds of
    // megabytes of native libraries just to discover two static version values.
    let script = r#"import importlib.metadata,importlib.util,json,pathlib,sys
tv=vf=None
try:
 tv=importlib.metadata.version('torch')
 spec=importlib.util.find_spec('torch')
 roots=list(spec.submodule_search_locations or []) if spec else []
 if roots: vf=str(pathlib.Path(roots[0])/'version.py')
except Exception:
 pass
print(json.dumps({'executable':sys.executable,'version':'.'.join(map(str,sys.version_info[:3])),'prefix':sys.prefix,'base_prefix':sys.base_prefix,'torch_distribution_version':tv,'torch_version_file':vf}))"#;
    let probe = command::output(&python, &["-c", script], cwd)
        .and_then(|value| serde_json::from_str::<Probe>(&value).ok());
    let mut pip_candidates = command::find_executables("pip3");
    pip_candidates.extend(command::find_executables("pip"));
    pip_candidates.dedup();
    let pip = pip_candidates.first().cloned();
    let pip_output = pip
        .as_deref()
        .and_then(|path| command::output(path, &["--version"], cwd));
    let (pip_version, pip_python_version) = pip_output
        .as_deref()
        .map(parse_pip_version)
        .unwrap_or_default();
    let module_pip_version = command::output(&python, &["-m", "pip", "--version"], cwd)
        .and_then(|output| parse_pip_version(&output).0);
    match probe {
        Some(p) => {
            let (torch_version, torch_cuda) = torch_values(&p);
            PythonInfo {
                torch_version,
                torch_cuda,
                executable: Some(p.executable.into()),
                version: Some(p.version),
                prefix: Some(p.prefix.into()),
                base_prefix: Some(p.base_prefix.into()),
                virtual_env,
                conda_prefix,
                pip_executable: pip,
                pip_version,
                pip_python_version,
                module_pip_version,
                executable_candidates: python_candidates,
                pip_candidates,
            }
        }
        None => PythonInfo {
            executable: Some(python),
            virtual_env,
            conda_prefix,
            pip_executable: pip,
            pip_version,
            pip_python_version,
            module_pip_version,
            executable_candidates: python_candidates,
            pip_candidates,
            ..Default::default()
        },
    }
}

fn torch_values(probe: &Probe) -> (Option<String>, Option<String>) {
    let from_file = probe
        .torch_version_file
        .as_deref()
        .and_then(|path| command::read_text(Path::new(path), 64 * 1024))
        .map(|contents| parse_torch_version_file(&contents))
        .unwrap_or_default();
    (
        from_file
            .0
            .or_else(|| probe.torch_distribution_version.clone()),
        from_file.1,
    )
}

fn parse_torch_version_file(contents: &str) -> (Option<String>, Option<String>) {
    let mut version = None;
    let mut cuda = None;
    for line in contents.lines() {
        let Some((left, right)) = line.split_once('=') else {
            continue;
        };
        let name = left.split(':').next().unwrap_or(left).trim();
        let value = right.trim();
        let literal = if value == "None" {
            None
        } else {
            value
                .strip_prefix(['\'', '"'])
                .and_then(|value| value.strip_suffix(['\'', '"']))
                .map(str::to_owned)
        };
        match name {
            "__version__" => version = literal,
            "cuda" => cuda = literal,
            _ => {}
        }
    }
    (version, cuda)
}

fn parse_pip_version(output: &str) -> (Option<String>, Option<String>) {
    let fields = output.split_whitespace().collect::<Vec<_>>();
    let version = (fields.first() == Some(&"pip"))
        .then(|| fields.get(1).copied().map(str::to_owned))
        .flatten();
    let python = output
        .rsplit_once("(python ")
        .and_then(|(_, tail)| tail.strip_suffix(')'))
        .map(str::to_owned);
    (version, python)
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_pip_version() {
        assert_eq!(
            super::parse_pip_version(
                "pip 25.1 from /venv/lib/python3.12/site-packages/pip (python 3.12)"
            ),
            (Some("25.1".into()), Some("3.12".into()))
        );
    }

    #[test]
    fn parses_torch_generated_version_file() {
        let contents = "__version__ = '2.9.1+cu128'\ndebug = False\ncuda: Optional[str] = '12.8'\n";
        assert_eq!(
            super::parse_torch_version_file(contents),
            (Some("2.9.1+cu128".into()), Some("12.8".into()))
        );
    }
}
