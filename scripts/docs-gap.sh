#!/usr/bin/env bash
# docs-gap.sh — PR の差分からドキュメントの更新漏れ候補を機械的に洗い出す（依存: git, POSIX grep/sed）
#
# 「コードを変えたのに対応するドキュメントが同じ PR に無い」は okf-lint では捕まらない
# （okf-lint は存在するドキュメントしか検査しない）。そのうち機械的に判定できる部分を
# ここで出し、判断が要る部分（古い説明が残っていないか等）は呼び出し側と Copilot に任せる。
#
# チェック内容:
#   1. 新しい crate / plugin / service に ai-docs/components/<name>.md が無い
#   2. lib.rs に足した `pub mod` が、その crate の components ドキュメントに名前として出てこない
#   3. ai-docs/ を変えたのに新しい ai-docs/log.d/ 断片が無い
#   4. 設定キーを足したのに config-reference.md に無い／消したキーが config-reference.md・
#      totsuka-config スキルにまだ出てくる。キーは config-template-lint.sh --code-keys で
#      base と head の両ツリーから抜き、集合の差を取る（非 pub の Raw… 型・serde(rename)・
#      flatten も同じ規則で扱い、型変更や struct 間の移動は差に出ない）
#   5. （参考）変更したパスを frontmatter の resource に持つが、この差分に入っていない concept。
#      ADR・リリースノート・log.d は過去の記録なので除く。各ファイルを読んで要否を判断する
#   6. 新しい task source / agent IDE プラグインが totsuka-config スキルに無い
#
# 使い方: scripts/docs-gap.sh            # main...HEAD
#         range=<rev-range> scripts/docs-gap.sh
# 終了コード: 1〜4・6 に 1 件以上あれば 1、無ければ 0（5 は件数に数えない）。
#           差分やキーを取れないときは 2（検査できないことを「漏れなし」にしない）
set -u

range=${range:-main...HEAD}
changed=$(git diff --name-only "${range}") || {
  echo "docs-gap: git diff ${range} に失敗" >&2
  exit 2
}
case "${range}" in
  *...*) head_rev=${range#*...} base_rev=$(git merge-base "${range%%...*}" "${head_rev:-HEAD}") ;;
  *..*) head_rev=${range#*..} base_rev=${range%%..*} ;;
  *) head_rev=HEAD base_rev=${range} ;;
esac
head_rev=${head_rev:-HEAD}

# <rev> の config struct のソースだけを一時ディレクトリへ書き出し、そのキーを出す
tmp=$(mktemp -d)
trap 'rm -rf "${tmp}"' EXIT
config_keys() {
  local dir="${tmp}/$1"
  git ls-tree -r --name-only "$1" -- crates/orchestrator-core/src/config/schema.rs plugins |
    grep -E '^(crates/orchestrator-core/src/config/schema\.rs|plugins/[^/]+/src/config\.rs)$' |
    while read -r f; do
      mkdir -p "${dir}/$(dirname "${f}")" && git show "$1:${f}" >"${dir}/${f}"
    done
  bash scripts/config-template-lint.sh --code-keys "${dir}"
}
base_keys=$(config_keys "${base_rev}") && head_keys=$(config_keys "${head_rev}") || {
  echo "docs-gap: 設定キーの抽出に失敗" >&2
  exit 2
}
new_crates=$(git diff --name-only --diff-filter=A "${range}" -- '*/*/Cargo.toml' |
  xargs -n1 dirname | xargs -n1 basename)

findings=$(
  echo "### 1. new crate / plugin / service without ai-docs/components/<name>.md"
  echo "${new_crates}" | grep . |
    while read -r n; do
      test -f "ai-docs/components/${n}.md" || echo "missing: ai-docs/components/${n}.md"
    done

  echo "### 2. new pub mod not named in its component doc"
  git diff --name-only "${range}" -- '*/*/src/lib.rs' | while read -r f; do
    n=$(basename "$(dirname "$(dirname "${f}")")")
    git diff "${range}" -- "${f}" | sed -nE 's/^\+[[:space:]]*pub mod ([a-z_0-9]+).*/\1/p' |
      while read -r m; do
        grep -qw "${m}" "ai-docs/components/${n}.md" 2>/dev/null || echo "missing: ${n}::${m}"
      done
  done

  echo "### 3. ai-docs changed without a new log.d fragment"
  if echo "${changed}" | grep -q '^ai-docs/' &&
    ! git diff --name-only --diff-filter=A "${range}" -- 'ai-docs/log.d/' | grep -q .; then
    echo "missing: ai-docs/log.d/YYYY-MM-DD-<slug>.md"
  fi

  echo "### 4. config keys: added but not in config-reference.md / removed but still named"
  comm -13 <(echo "${base_keys}") <(echo "${head_keys}") | grep . |
    while read -r k; do
      grep -qw "${k}" ai-docs/development/config-reference.md || echo "missing: ${k}"
    done
  comm -23 <(echo "${base_keys}") <(echo "${head_keys}") | grep . |
    while read -r k; do
      git grep -n -w "${k}" -- ai-docs/development/config-reference.md skills/totsuka-config |
        sed "s/^/still named (${k}): /"
    done

  echo "### 5. (read each) concepts whose resource is a changed path, not in this diff"
  echo "${changed}" | grep -E '^(crates|plugins|services|apps)/' | cut -d/ -f1-2 | sort -u |
    while read -r p; do bash scripts/okf-search.sh --resource-like "${p}" --paths-only 2>/dev/null; done |
    grep -vE '^ai-docs/(decisions|releases|log\.d)/' | sort -u | grep -vxF "${changed}"

  echo "### 6. new task source / agent IDE plugin missing from the totsuka-config skill"
  echo "${new_crates}" | grep . | while read -r n; do
    s=${n#task-source-}
    s=${s#agent-ide-}
    case "${n}" in
    task-source-*)
      test -f "skills/totsuka-config/references/${s}.md" ||
        echo "missing: skills/totsuka-config/references/${s}.md"
      ;;
    agent-ide-*) ;;
    *) continue ;;
    esac
    grep -qw "${s}" skills/totsuka-config/SKILL.md || echo "missing in SKILL.md: ${s}"
  done
)

echo "${findings}"
! echo "${findings}" | grep -qE '^(missing|still named)'
