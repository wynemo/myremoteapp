# 项目约定

## 构建与验证

- 不要执行 Windows 编译，包括交叉编译、Windows target 的 `cargo build` / `cargo check` / `cargo test`，以及通过 CI、虚拟机或远程机器触发的 Windows 构建。
- 可以编写和审查 Windows 端代码，但验证仅限当前 macOS 环境可运行的共享模块和 macOS 端代码。
- 汇报验证结果时，明确说明 Windows 端未编译、未运行，不要将 macOS 上的验证描述为 Windows 验证通过。
- 只有用户后续明确授权 Windows 编译时，才能调整上述限制。
