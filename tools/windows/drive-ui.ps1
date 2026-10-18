param([string]$Window = 'Mouse Blur', [string]$Invoke, [string]$Toggle, [switch]$List, [string]$Find)
# Drives the Mouse Blur settings window through Windows UI Automation, which egui exposes via
# AccessKit. Nothing moves the mouse. Examples:
#   .\tools\windows\drive-ui.ps1 -List
#   .\tools\windows\drive-ui.ps1 -Invoke 'Check for updates'
#   .\tools\windows\drive-ui.ps1 -Find 'Version*'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$A = [System.Windows.Automation.AutomationElement]
$root = $A::RootElement
$cond = New-Object System.Windows.Automation.PropertyCondition($A::NameProperty, $Window)
$win = $null
for ($i = 0; $i -lt 20 -and -not $win; $i++) {
  $win = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $cond)
  if (-not $win) { Start-Sleep -Milliseconds 250 }
}
if (-not $win) { Write-Output "window '$Window' not found"; exit 1 }
function All { $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) }
# AccessKit builds its tree on first query; give it a moment to populate.
for ($i = 0; $i -lt 20 -and (All).Count -lt 5; $i++) { Start-Sleep -Milliseconds 250 }
function ByName($name) {
  foreach ($e in All) { if ($e.Current.Name -eq $name) { return $e } }
  return $null
}
if ($List) {
  foreach ($e in All) {
    $n = $e.Current.Name; if ($n) { "{0,-14} {1}" -f $e.Current.ControlType.ProgrammaticName.Replace('ControlType.', ''), $n }
  }
}
if ($Find) {
  foreach ($e in All) { if ($e.Current.Name -like $Find) { $e.Current.Name } }
}
if ($Invoke) {
  $e = ByName $Invoke
  if (-not $e) { Write-Output "no element named '$Invoke'"; exit 2 }
  $e.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
  Write-Output "invoked '$Invoke'"
}
if ($Toggle) {
  $e = ByName $Toggle
  if (-not $e) { Write-Output "no element named '$Toggle'"; exit 2 }
  $e.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
  Write-Output "toggled '$Toggle'"
}
