//! What a Windows host is asked, in PowerShell, because OpenSSH there runs
//! commands with `cmd.exe` and the POSIX scripts the other hosts receive are
//! not a language it speaks.
//!
//! The script travels as `-EncodedCommand`, which is the script in UTF-16
//! little-endian and then Base64. That keeps a prefix containing a quote or a
//! space from being re-read by `cmd`.

use std::path::Path;

use crate::bootstrap::upload::{DIGEST_VARIABLE, PREFIX_VARIABLE};

/// The probe. It prints the same fields the POSIX probe prints.
pub const PROBE_SCRIPT: &str = r#"
$ErrorActionPreference = 'Continue'
Write-Output 'system Windows_NT'
$machine = $env:PROCESSOR_ARCHITECTURE
if (-not $machine) { $machine = '-' }
Write-Output "machine $machine"
Write-Output 'tic no'
$local = $env:LOCALAPPDATA
$home = $env:USERPROFILE
$temp = $env:TEMP
if (-not $local) { $local = $home }
$candidates = @(
  (Join-Path $local 'iznik'),
  (Join-Path $home '.local\share\iznik'),
  (Join-Path $temp 'iznik')
)
$index = 0
foreach ($candidate in $candidates) {
  $executable = Join-Path $candidate 'bin\iznik-server.exe'
  $said = '-'
  if (Test-Path -LiteralPath $executable) {
    $said = (& $executable --version 2>$null | Select-Object -First 1)
    if (-not $said) { $said = '-' }
  }
  $writable = 'no'
  $look = $candidate
  while ($look -and -not (Test-Path -LiteralPath $look)) {
    $look = Split-Path -Path $look -Parent
  }
  foreach ($root in @($local, $home, $temp)) {
    if ($root -and $look -and $look.StartsWith($root, [System.StringComparison]::OrdinalIgnoreCase)) {
      $writable = 'yes'
    }
  }
  Write-Output "candidate $index writable $writable"
  Write-Output "candidate $index version $said"
  Write-Output "candidate $index terminfo no"
  Write-Output "candidate $index path $candidate"
  $index = $index + 1
}
"#;

/// The upload. The bytes arrive on standard input. A wrong digest exits 65,
/// which is the code the client already treats as a digest refusal.
pub const UPLOAD_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$prefix = $env:IZNIK_PREFIX
$into = Join-Path $prefix 'bin'
New-Item -ItemType Directory -Force -Path $into | Out-Null
$partial = Join-Path $into ('.partial-' + [guid]::NewGuid().ToString('N'))
$incoming = [Console]::OpenStandardInput()
$outgoing = [System.IO.File]::Create($partial)
$incoming.CopyTo($outgoing)
$outgoing.Close()
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $partial).Hash.ToLowerInvariant()
$expected = $env:IZNIK_DIGEST
if (-not $expected) { $expected = '' }
if ($hash -ne $expected.ToLowerInvariant()) {
  Remove-Item -Force -LiteralPath $partial
  [Console]::Error.WriteLine("digest $hash")
  exit 65
}
$destination = Join-Path $into 'iznik-server.exe'
Move-Item -Force -LiteralPath $partial -Destination $destination
Write-Output "installed $destination"
"#;

/// Stops a daemon, if one is there. A host with no server is not a failure.
pub const STOP_SCRIPT: &str = r#"
$server = Join-Path $env:IZNIK_PREFIX 'bin\iznik-server.exe'
if (Test-Path -LiteralPath $server) { & $server --stop *> $null }
Write-Output "stopped $($env:IZNIK_PREFIX)"
"#;

/// Removes what iznik installed and the daemon's runtime directory.
pub const UNINSTALL_SCRIPT: &str = r#"
$prefix = $env:IZNIK_PREFIX
$server = Join-Path $prefix 'bin\iznik-server.exe'
if (Test-Path -LiteralPath $server) { & $server --stop *> $null }
Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath $server
$bin = Join-Path $prefix 'bin'
if (Test-Path -LiteralPath $bin) {
  Get-ChildItem -LiteralPath $bin -Filter '.partial-*' -ErrorAction SilentlyContinue | Remove-Item -Force
}
$runtime = Join-Path $env:LOCALAPPDATA 'iznik'
if (-not $env:LOCALAPPDATA) { $runtime = Join-Path $env:TEMP 'iznik' }
Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -LiteralPath $runtime
Write-Output "removed $prefix"
Write-Output "runtime $runtime"
"#;

/// The probe as one remote command.
#[must_use]
pub fn probe_command() -> String {
    encoded_shell(PROBE_SCRIPT)
}

/// One remote command that sets `IZNIK_PREFIX` and then runs `script`.
#[must_use]
pub fn command_for(script: &str, prefix: &Path) -> String {
    format!(
        "set \"{PREFIX_VARIABLE}={prefix}\"& {encoded}",
        prefix = prefix.display(),
        encoded = encoded_shell(script)
    )
}

/// The upload as one remote command, with `IZNIK_PREFIX` and `IZNIK_DIGEST` set.
#[must_use]
pub fn upload_command(prefix: &Path, digest: &str) -> String {
    format!(
        "set \"{PREFIX_VARIABLE}={prefix}\"& set \"{DIGEST_VARIABLE}={digest}\"& {encoded}",
        prefix = prefix.display(),
        encoded = encoded_shell(UPLOAD_SCRIPT)
    )
}

/// `powershell.exe -EncodedCommand` for `script`.
fn encoded_shell(script: &str) -> String {
    format!(
        "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
        encoded(script)
    )
}

/// UTF-16LE Base64, which is what `-EncodedCommand` reads.
fn encoded(script: &str) -> String {
    let wide: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    crate::base64::encode(&wide)
}
