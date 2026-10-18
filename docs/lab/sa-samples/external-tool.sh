#!/usr/bin/env bash
# external-tool: from outside the cluster, list Pods in al-sample every 60 s with the legacy
# Secret token. The token is read once into a variable and passed to curl on stdin; it is
# never in argv, a file or the output.
# Usage: external-tool.sh <admin kubeconfig> <apiserver URL>     (stop with kill)
set -euo pipefail
KC=${1:?admin kubeconfig}; API=${2:?apiserver URL, e.g. https://<node>:6443}
T=$(kubectl --kubeconfig "$KC" -n al-sample get secret external-tool-token -o jsonpath='{.data.token}' | base64 -d)
CA=$(kubectl --kubeconfig "$KC" -n al-sample get secret external-tool-token -o jsonpath='{.data.ca\.crt}' | base64 -d)
[ -n "$T" ] || { echo "no token in external-tool-token"; exit 1; }
while true; do
  code=$(printf 'Authorization: Bearer %s\n' "$T" \
    | curl -sS -o /dev/null -w '%{http_code}' --cacert <(printf '%s\n' "$CA") \
        -A "external-tool-sample/1.0 (legacy token, outside the cluster)" -H @- \
        "$API/api/v1/namespaces/al-sample/pods")
  echo "$(date -u +%FT%TZ) $code"
  sleep 60
done
