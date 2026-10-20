# Installs Kargo, Cargo for Kotlin, on Windows:
#
#   irm https://enekocm7.github.io/kargo/install.ps1 | iex
#
# `irm | iex` cannot pass arguments, so the options are environment variables:
#   KARGO_VERSION          the version to install (default: the latest release)
#   KARGO_HOME             where Kargo lives (default %USERPROFILE%\.kargo); the binary goes to %KARGO_HOME%\bin
#   KARGO_NO_MODIFY_PATH   set to 1 to leave the user PATH alone
#   KARGO_YES              set to 1 to add Kargo to the user PATH without asking
#   KARGO_RELEASES_URL     where releases are published (default https://github.com/enekocm7/kargo/releases;
#                          a mirror, or a directory with file://)
#   KARGO_TARGET           install the build for this target instead of the detected one
#
# The archive's SHA-256 is checked against the release's SHA256SUMS before anything is installed.

& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    # Windows PowerShell 5.1 does not offer TLS 1.2 to GitHub unless asked to.
    try { [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12 } catch {}

    function Say([string]$Message) { [Console]::Error.WriteLine("kargo-install: $Message") }
    function Fail([string]$Message) { throw "kargo-install: error: $Message" }

    $releases = if ($env:KARGO_RELEASES_URL) { $env:KARGO_RELEASES_URL } else { 'https://github.com/enekocm7/kargo/releases' }
    $releases = $releases.TrimEnd('/')
    $version = if ($env:KARGO_VERSION) { $env:KARGO_VERSION.TrimStart('v') } else { '' }
    $home_dir = if ($env:KARGO_HOME) { $env:KARGO_HOME } else { Join-Path $env:USERPROFILE '.kargo' }
    $bin_dir = Join-Path $home_dir 'bin'

    # The target triple of the build to install.
    if ($env:KARGO_TARGET) {
        $target = $env:KARGO_TARGET
    } else {
        $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
        switch ($arch) {
            'X64' { $target = 'x86_64-pc-windows-msvc' }
            'Arm64' { $target = 'aarch64-pc-windows-msvc' }
            default { Fail "no prebuilt Kargo for the $arch architecture; build it from source: https://enekocm7.github.io/kargo/getting-started/installation/" }
        }
    }

    # Downloads $Url to $Path; a file:// URL (a local release directory) is copied.
    function Fetch([string]$Url, [string]$Path) {
        if ($Url.StartsWith('file:')) {
            Copy-Item -LiteralPath ([Uri]$Url).LocalPath -Destination $Path
        } else {
            Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Path
        }
    }

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("kargo-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        if (-not $version) {
            Say 'looking up the latest release'
            try { Fetch "$releases/latest/download/VERSION" "$tmp\VERSION" } catch { Fail "could not find the latest release at $releases" }
            $version = (Get-Content -Raw "$tmp\VERSION").Trim().TrimStart('v')
        }
        if (-not $version) { Fail 'the latest release has no version' }

        $archive = "kargo-$version-$target.zip"
        $base = "$releases/download/v$version"
        Say "installing kargo $version for $target"
        try { Fetch "$base/SHA256SUMS" "$tmp\SHA256SUMS" } catch { Fail "could not find release v$version at $base" }
        $expected = $null
        foreach ($line in Get-Content "$tmp\SHA256SUMS") {
            $parts = $line.Trim() -split '\s+', 2
            if ($parts.Count -eq 2 -and $parts[1].TrimStart('*') -eq $archive) { $expected = $parts[0].ToLowerInvariant() }
        }
        if (-not $expected) { Fail "release v$version has no build for $target ($archive)" }

        try { Fetch "$base/$archive" "$tmp\$archive" } catch { Fail "could not download $base/$archive" }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath "$tmp\$archive").Hash.ToLowerInvariant()
        if ($actual -ne $expected) { Fail "checksum mismatch for ${archive}: expected $expected, got $actual" }

        Expand-Archive -LiteralPath "$tmp\$archive" -DestinationPath "$tmp\unpacked"
        $binary = Join-Path "$tmp\unpacked\kargo-$version-$target" 'kargo.exe'
        if (-not (Test-Path -LiteralPath $binary)) { $binary = Join-Path "$tmp\unpacked" 'kargo.exe' }
        if (-not (Test-Path -LiteralPath $binary)) { Fail 'the archive does not contain kargo.exe' }

        New-Item -ItemType Directory -Force -Path $bin_dir | Out-Null
        $exe = Join-Path $bin_dir 'kargo.exe'
        if (Test-Path -LiteralPath $exe) {
            # A running kargo.exe cannot be overwritten, but it can be renamed out of the way.
            $old = "$exe.old"
            Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
            Move-Item -LiteralPath $exe -Destination $old -Force
            Remove-Item -LiteralPath $old -Force -ErrorAction SilentlyContinue
        }
        Copy-Item -LiteralPath $binary -Destination $exe
        Say "installed $exe"
    } finally {
        Remove-Item -Recurse -Force -LiteralPath $tmp -ErrorAction SilentlyContinue
    }

    # Puts the bin directory on the user's PATH (with consent), and in this session.
    function Same([string]$A, [string]$B) { $A.TrimEnd('\') -ieq $B.TrimEnd('\') }
    $user_path = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = @(); if ($user_path) { $entries = $user_path -split ';' | Where-Object { $_ } }
    $on_path = [bool]($entries | Where-Object { Same $_ $bin_dir })
    if ($on_path) {
        # Nothing to do.
    } elseif ($env:KARGO_NO_MODIFY_PATH -eq '1') {
        Say "$bin_dir is not in your PATH; add it with: setx PATH `"%PATH%;$bin_dir`""
    } else {
        $answer = $true
        if ($env:KARGO_YES -ne '1') {
            if ([Environment]::UserInteractive -and -not [Console]::IsInputRedirected) {
                $reply = Read-Host "kargo-install: add $bin_dir to your user PATH? [Y/n]"
                if ($reply -match '^(n|no)$') { $answer = $false }
            } else {
                $answer = $false
                Say 'no terminal to ask on, so your PATH stays as it is (set KARGO_YES=1 to change it)'
            }
        }
        if ($answer) {
            $new_path = (@($entries) + $bin_dir) -join ';'
            [Environment]::SetEnvironmentVariable('Path', $new_path, 'User')
            Say "added $bin_dir to your user PATH; open a new terminal to use kargo"
        } else {
            Say "add $bin_dir to your PATH to run kargo from anywhere"
        }
    }
    if (-not ($env:Path -split ';' | Where-Object { $_ -and (Same $_ $bin_dir) })) { $env:Path = "$env:Path;$bin_dir" }

    try { & $exe --version } catch { Say 'warning: the installed kargo did not start' }
    Say 'next: kargo new hello; cd hello; kargo run'
}
