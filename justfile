# 默认列出所有命令
default:
    @just --list

# 编译（debug）
build:
    cargo build

# 编译并签名（Keychain / Touch ID 需要）
build-signed: build sign

# 签名所有测试二进制（Keychain 测试需要）
sign-tests:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo test --workspace --no-run --message-format=json \
      | jq -r 'select(.executable != null) | .executable' \
      | while read -r bin; do
          codesign --force --sign - --entitlements ares.entitlements "$bin"
        done
    echo "✓ signed all test binaries"

# 运行全部测试（先签名，Keychain/TouchID 测试依赖）
# --test-threads=1 的原因见 Task 0 Step 7 注释
test: build sign sign-tests
    cargo test --workspace -- --test-threads=1

# 静态检查
check:
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings

# 签名 debug 二进制
sign:
    codesign --force --sign - \
        --entitlements ares.entitlements \
        --options runtime \
        target/debug/ares
    @echo "✓ signed target/debug/ares"

# 验证签名与 entitlements
verify-sign:
    codesign -dv --entitlements - target/debug/ares
