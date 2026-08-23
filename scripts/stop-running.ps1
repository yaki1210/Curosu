$ErrorActionPreference = "Stop"

Add-Type -MemberDefinition @'
[DllImport("user32.dll")]
public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);
public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);

[DllImport("user32.dll")]
public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

[DllImport("user32.dll")]
public static extern bool IsWindowVisible(IntPtr hWnd);

[DllImport("user32.dll")]
public static extern bool PostMessage(IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);
'@ -Name Native -Namespace CurosuStop

$processes = Get-Process curosu -ErrorAction SilentlyContinue
if (-not $processes)
{
    Write-Host "No curosu process is running."
    exit 0
}

foreach ($process in $processes)
{
    $targetPid = $process.Id
    $callback = [CurosuStop.Native+EnumWindowsProc]{
        param([IntPtr]$hWnd, [IntPtr]$lParam)
        [uint32]$winPid = 0
        [CurosuStop.Native]::GetWindowThreadProcessId($hWnd, [ref]$winPid) | Out-Null
        if ($winPid -eq $targetPid -and [CurosuStop.Native]::IsWindowVisible($hWnd))
        {
            [CurosuStop.Native]::PostMessage($hWnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
        }
        return $true
    }
    [CurosuStop.Native]::EnumWindows($callback, [IntPtr]::Zero) | Out-Null
}

Start-Sleep -Milliseconds 1200
$remaining = Get-Process curosu -ErrorAction SilentlyContinue
if ($remaining)
{
    # 覆盖层通常没有可见顶级窗口，因而收不到上面的 WM_CLOSE。
    # 安装前必须释放 exe 文件句柄；在已经等待过优雅退出后再强制终止。
    Write-Host "Some curosu processes are still running; forcing termination."
    $remaining | Stop-Process -Force -ErrorAction Stop
    Start-Sleep -Milliseconds 500

    $remaining = Get-Process curosu -ErrorAction SilentlyContinue
    if ($remaining)
    {
        throw "Unable to stop all curosu processes."
    }

    Write-Host "curosu force-stopped."
}
else
{
    Write-Host "curosu stopped."
}
