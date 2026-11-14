$ErrorActionPreference = 'Stop'

# Discover the compiled test executable through Cargo. Apply the smaller stack
# only to the test process: rustc itself needs its normal compiler thread stack.
$messages = & cargo test --locked --manifest-path src-tauri/Cargo.toml --lib --no-run --message-format=json
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$testExecutables = @($messages | ForEach-Object { $_ | ConvertFrom-Json } | Where-Object {
    $_.reason -eq 'compiler-artifact' -and $_.target.name -eq 'rillsql_lib' -and $_.profile.test -and $_.executable
} | ForEach-Object { $_.executable })
if ($testExecutables.Count -ne 1) {
    throw "Expected one library test executable, found $($testExecutables.Count)"
}

$previousStack = $env:RUST_MIN_STACK
try {
    $env:RUST_MIN_STACK = '786432'
    & $testExecutables[0] 'app::views::diagram::tests::diagram_four_states_and_keyboard_hold_at_1360' --exact --nocapture
    $testExitCode = $LASTEXITCODE
} finally {
    $env:RUST_MIN_STACK = $previousStack
}
exit $testExitCode
