# Wiring kube-apiserver to audit-trail (RKE2)

Untested sketch; verify against the RKE2 and Rancher docs for your version before use.
A wrong path or an unmounted file stops kube-apiserver from starting.

Files on every control-plane node:

| Path | From |
|---|---|
| `/etc/rancher/rke2/audit-policy.yaml` | `audit-policy.yaml` |
| `/etc/rancher/rke2/audit-webhook.yaml` | `webhook-kubeconfig.yaml` with `CLUSTER_IP` filled in |
| `/etc/rancher/rke2/audit-trail.token` | token from Secret `cattle-audit-trail/audit-trail-ingest`, mode 600 |

RKE2 server config (`/etc/rancher/rke2/config.yaml` or a drop-in under `config.yaml.d/`):

```yaml
audit-policy-file: /etc/rancher/rke2/audit-policy.yaml
kube-apiserver-arg+:
  - audit-webhook-config-file=/etc/rancher/rke2/audit-webhook.yaml
  - audit-webhook-batch-max-wait=5s
  - audit-webhook-batch-max-size=100
kube-apiserver-extra-mount+:
  - /etc/rancher/rke2/audit-webhook.yaml:/etc/rancher/rke2/audit-webhook.yaml:ro
  - /etc/rancher/rke2/audit-trail.token:/etc/rancher/rke2/audit-trail.token:ro
```

Use the `+` forms so the keys add to, rather than replace, values Rancher already sets in
`config.yaml.d/50-rancher.yaml`. For Rancher-provisioned clusters, set the equivalent in the
cluster's `machineSelectorConfig` instead of editing nodes by hand (Rancher would overwrite it).

After restarting `rke2-server`, confirm:

- `/var/lib/rancher/rke2/agent/pod-manifests/kube-apiserver.yaml` contains the three flags and both mounts.
- `kubectl get --raw /readyz` returns `ok`.
- `curl http://CLUSTER_IP:8081/healthz` from the node returns `ok`.

Keep a copy of the previous `config.yaml` (and drop-ins) to roll back.
