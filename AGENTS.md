# 项目约定

## 构建与验证

- 不在 macOS 本机安装 Windows 工具链、SDK 或交叉编译 Windows；本机只验证 macOS 与共享模块。
- 用户已授权通过 GitHub Actions 的 Windows runner 原生构建和验证 Windows 产物。Windows 构建交给 CI，不使用本机交叉编译、虚拟机或其他远程机器替代。
- CI 的构建、测试及 Clippy 使用 Release 模式，依赖使用 `--locked`。
- 汇报时区分本机 macOS 验证与 Windows CI 验证；只有 Windows CI 实际通过后，才能声称 Windows 构建或测试通过。
