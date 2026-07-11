set -e

echo '>>> ===== starting push... ===== <<<'
git add . && git status
# shellcheck disable=SC2162
read -t 100 -p "[commit] >>> " commit_msg
if [ "$commit_msg" == "" ]; then
  git reset
  echo "[main] You haven't entered any comments !"
  exit 1
else
  git commit -m "[commit]: $commit_msg"
  git pull origin main
  git push origin main
  exit 0
fi
echo '>>> ===== push success! ===== <<<'