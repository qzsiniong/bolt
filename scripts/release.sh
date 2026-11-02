#!/usr/bin/env bash
#
# Bolt 发布新版：改版本号 → 同步 Cargo.lock → 跑本地门禁 →（可选）提交 / 打 tag / 推送。
#
# 版本号的真源是 **根 Cargo.toml**（`bolt` 包的 version）：release 工作流拿它与
# tag 比对，打包脚本也从中读出并注入 Info.plist。其余 5 个 workspace 成员
# crate 不单独发布，但版本号顺带保持一致；Cargo.lock 由 cargo 自己同步。
#
# 用法:
#   scripts/release.sh <版本|patch|minor|major> [选项]
#
# 例子:
#   scripts/release.sh patch                     # 0.1.0 → 0.1.1，跑门禁，然后交互问要不要提交/打 tag/推送
#   scripts/release.sh 0.2.0-beta.1 --dry-run     # 只打印将要改什么
#   scripts/release.sh minor --commit             # 只提交，不打扰（不打 tag、不推）
#   scripts/release.sh 0.2.0 --commit --tag --push  # 全自动（CI 与发版流程会被触发）
#
# 选项:
#   --commit        提交版本号改动（commit message: chore(release): vX.Y.Z）
#   --tag           打注释 tag vX.Y.Z（需要同时 --commit）
#   --push          推当前分支与 tag（需要同时 --commit --tag；会触发 CI 与发版）
#   --yes, -y       等价于 --commit --tag --push（谨慎）
#   --no-check      跳过本地门禁（默认跑与 CI 相同的那套）
#   --dry-run       只打印将要改什么，不写任何文件
#   --allow-dirty   工作区不干净时也继续（默认拒绝，避免把无关改动混进发版提交）
#   -h, --help      显示本帮助
#
# 不给 --commit/--tag/--push 时：如果是在终端里跑，就逐条交互询问；否则只改版本号并打印下一步命令。
# 非 TTY 环境（IDE 任务、CI 包装脚本）也想走交互：设 BOLT_RELEASE_INTERACTIVE=1。

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# 只有这些文件记录版本号（外加 Cargo.lock，由 cargo 自己同步）
CARGO_FILES=(
  Cargo.toml
  crates/bolt-console/Cargo.toml
  crates/bolt-extensions/Cargo.toml
  crates/bolt-lsp/Cargo.toml
  crates/bolt-model/Cargo.toml
  crates/bolt-ts/Cargo.toml
)

usage() {
  sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

target=""
do_commit=0
do_tag=0
do_push=0
yes_all=0
no_check=0
dry_run=0
allow_dirty=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h | --help)
      usage
      exit 0
      ;;
    --commit) do_commit=1 ;;
    --tag) do_tag=1 ;;
    --push) do_push=1 ;;
    --yes | -y) yes_all=1 ;;
    --no-check) no_check=1 ;;
    --dry-run) dry_run=1 ;;
    --allow-dirty) allow_dirty=1 ;;
    -*)
      echo "✗ 未知选项：$1" >&2
      usage >&2
      exit 2
      ;;
    *)
      [[ -z "${target}" ]] || {
        echo "✗ 只接受一个版本参数（收到：${target} 与 $1）" >&2
        exit 2
      }
      target="$1"
      ;;
  esac
  shift
done

if [[ -z "${target}" ]]; then
  usage >&2
  exit 2
fi

if [[ $yes_all -eq 1 ]]; then
  do_commit=1
  do_tag=1
  do_push=1
fi
if [[ $do_tag -eq 1 && $do_commit -eq 0 ]]; then
  echo "✗ --tag 需要同时给 --commit：否则 tag 会指向「还没包含版本号改动」的提交" >&2
  exit 2
fi
if [[ $do_push -eq 1 && $do_tag -eq 0 ]]; then
  echo "✗ --push 需要同时给 --commit --tag" >&2
  exit 2
fi

# ── 当前版本 → 目标版本 ──────────────────────────────────────────────────────
current="$(sed -nE 's/^version = "([^"]+)"/\1/p' Cargo.toml | head -1)"
if [[ -z "${current}" ]]; then
  echo "✗ 读不到根 Cargo.toml 里的 version" >&2
  exit 1
fi

case "${target}" in
  patch | minor | major)
    base="${current%%-*}"
    IFS=. read -r major_ver minor_ver patch_ver <<<"$base"
    case "${target}" in
      major)
        major_ver=$((major_ver + 1))
        minor_ver=0
        patch_ver=0
        ;;
      minor)
        minor_ver=$((minor_ver + 1))
        patch_ver=0
        ;;
      patch) patch_ver=$((patch_ver + 1)) ;;
    esac
    new="$major_ver.$minor_ver.$patch_ver"
    ;;
  *) new="${target}" ;;
esac

if [[ ! "${new}" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
  echo "✗ 版本号不合法：${new}（示例：0.2.0 / 0.2.0-beta.1 / patch）" >&2
  exit 2
fi
if [[ "${new}" == "${current}" ]]; then
  echo "✗ 当前已经是 ${current}。" >&2
  echo "  要重打 tag 就直接用 git；要把文件改回来用：git checkout -- ${CARGO_FILES[*]} Cargo.lock" >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/v${new}" >/dev/null; then
  echo "✗ 本地已存在 tag v${new}：换个版本号，或先 git tag -d v${new}" >&2
  exit 1
fi
# 远端 tag 只在**真要推送**时查：这是网络调用（可能几十秒甚至卡住），
# 平时不发版不该被它拖住。顺带给 git 一个低速超时，避免链接被黑洞时无限等。
if [[ $do_push -eq 1 ]] && git -c http.lowSpeedLimit=1000 -c http.lowSpeedTime=10 \
  ls-remote --exit-code --tags origin "v${new}" >/dev/null 2>&1; then
  echo "✗ 远端已存在 tag v${new}：换个版本号，或先删掉远端 tag" >&2
  exit 1
fi

if [[ $allow_dirty -eq 0 ]]; then
  dirty="$(git status --porcelain)"
  if [[ -n "$dirty" ]]; then
    echo "✗ 工作区不干净 —— 发版提交里会混进无关改动。先提交/stash，或加 --allow-dirty：" >&2
    echo "$dirty" >&2
    exit 1
  fi
fi

echo "== Bolt 发布 =="
echo "  当前版本：v${current}"
echo "  目标版本：v${new}"
echo "  将改动：  ${CARGO_FILES[*]} + Cargo.lock"
if [[ $do_commit -eq 1 ]]; then echo "  之后：    提交$([[ $do_tag -eq 1 ]] && echo " → 打 tag v${new}")$([[ $do_push -eq 1 ]] && echo " → 推送（触发 CI 与发版）")"; fi

if [[ $dry_run -eq 1 ]]; then
  echo
  echo "（--dry-run：没有写任何文件）"
  exit 0
fi

restore_hint() {
  echo >&2
  echo "回退这次改动：git checkout -- ${CARGO_FILES[*]} Cargo.lock" >&2
}
trap 'echo "✗ 出错中断（版本号可能已被改动）" >&2; restore_hint' ERR

# ── 改版本号 ────────────────────────────────────────────────────────────────
# sed -i.bak 在 BSD(macOS) 与 GNU 上都可用，用完删掉备份。
bump_cargo() { sed -i.bak -E "s/^version = \"[^\"]+\"/version = \"$2\"/" "$1"; rm -f "$1.bak"; }

for f in "${CARGO_FILES[@]}"; do bump_cargo "$f" "${new}"; done
echo "✓ 已更新 ${#CARGO_FILES[@]} 个文件"

# ── 同步 Cargo.lock（只同步 workspace 成员版本，不动第三方依赖） ─────────────
log="$(mktemp -t bolt-release)"
if ! cargo update -w >"$log" 2>&1; then
  echo "✗ cargo update -w 失败（Cargo.lock 可能没同步成功）：" >&2
  tail -20 "$log" >&2
  restore_hint
  exit 1
fi
rm -f "$log"
echo "✓ Cargo.lock 已同步"

# ── 本地门禁（与 CI 相同） ───────────────────────────────────────────────────
if [[ $no_check -eq 1 ]]; then
  echo "→ 已跳过本地门禁（--no-check）"
else
  echo "== 本地门禁（与 CI 相同；跳过用 --no-check）=="
  for step in \
    "cargo test --workspace" \
    "cargo clippy --workspace --all-targets -- -D warnings"; do
    echo "→ $step"
    # shellcheck disable=SC2086 # 这里就是要按空格拆成命令与参数
    $step
  done
  echo "✓ 本地门禁通过"
fi

# ── 可选的提交 / 打 tag / 推送 ──────────────────────────────────────────────
explicit=0
if [[ $do_commit -eq 1 || $do_tag -eq 1 || $do_push -eq 1 ]]; then
  explicit=1
fi

if [[ $explicit -eq 0 ]]; then
  # 想在非 TTY 环境（IDE 任务、CI 包装脚本）里也走询问，就设 BOLT_RELEASE_INTERACTIVE=1
  if [[ -t 0 && -t 1 ]] || [[ "${BOLT_RELEASE_INTERACTIVE:-}" == "1" ]]; then
    echo
    echo "== 接下来（直接回车 = 用方括号里的默认值）=="
    read -r -p "提交这次版本号改动？[Y/n] " ans || true
    if [[ ! "$ans" =~ ^[Nn]$ ]]; then
      do_commit=1
      read -r -p "打 tag v${new}？[Y/n] " ans || true
      if [[ ! "$ans" =~ ^[Nn]$ ]]; then
        do_tag=1
        read -r -p "推送分支与 tag（会触发 CI 与发版流程）？[y/N] " ans || true
        if [[ "$ans" =~ ^[Yy]$ ]]; then do_push=1; fi
      fi
    fi
  else
    echo
    echo "（非交互环境：只改了版本号，下面是剩下的命令）"
  fi
fi

if [[ $do_commit -eq 1 ]]; then
  git add -- "${CARGO_FILES[@]}" Cargo.lock
  git commit -m "chore(release): v${new}"
  echo "✓ 已提交：chore(release): v${new}"
fi
if [[ $do_tag -eq 1 ]]; then
  git tag -a "v${new}" -m "Bolt v${new}"
  echo "✓ 已打 tag：v${new}"
fi
if [[ $do_push -eq 1 ]]; then
  branch="$(git rev-parse --abbrev-ref HEAD)"
  if [[ "${branch}" != "main" ]]; then
    echo "⚠ 当前分支是 ${branch}（不是 main）—— 确认这是你想发布的提交"
  fi
  git push
  git push origin "v${new}"
  echo "✓ 已推送分支与 tag"
fi

# ── 收尾提示 ────────────────────────────────────────────────────────────────
web_url="$(git remote get-url origin 2>/dev/null | sed -E 's#^git@([^:]+):#https://\1/#; s#\.git$##' || true)"

echo
echo "✓ 版本号：v${current} → v${new}"
if [[ $do_commit -eq 0 ]]; then
  echo "下一步（或重跑：scripts/release.sh ${new} --commit --tag --push 需先 git checkout -- .）："
  echo "  git add -- ${CARGO_FILES[*]} Cargo.lock"
  echo "  git commit -m \"chore(release): v${new}\""
fi
if [[ $do_tag -eq 0 && $do_commit -eq 1 ]]; then
  echo "  git tag -a v${new} -m \"Bolt v${new}\""
fi
if [[ $do_push -eq 0 && $do_tag -eq 1 ]]; then
  echo "  git push && git push origin v${new}"
fi
if [[ $do_push -eq 1 ]]; then
  echo
  echo "已推送：Release 工作流会自动跑 ci → verify → build（macOS universal dmg + Windows zip）"
  [[ -n "$web_url" ]] && echo "  看进度：$web_url/actions"
  [[ -n "$web_url" ]] && echo "  检查草稿并 Publish：$web_url/releases"
else
  echo
  echo "提示：推送 tag 才会触发发版；想先试跑流水线可用 Actions 页面的 Run workflow。"
fi
