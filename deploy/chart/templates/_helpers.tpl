{{- define "al.labels" -}}
app.kubernetes.io/name: audit-loupe
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end -}}

{{- define "al.selector" -}}
app.kubernetes.io/name: audit-loupe
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "al.serverImage" -}}
{{ .Values.image.server.repository }}:{{ .Values.image.server.tag | default .Chart.AppVersion }}
{{- end -}}

{{- define "al.collectorImage" -}}
{{ .Values.image.collector.repository }}:{{ .Values.image.collector.tag | default .Chart.AppVersion }}
{{- end -}}

{{- define "al.tokenSecret" -}}
{{- if .Values.collector.token.existingSecret -}}
{{ .Values.collector.token.existingSecret }}
{{- else -}}
audit-loupe-collector-token
{{- end -}}
{{- end -}}

{{/* Hardened container defaults: non-root, read-only root, no capabilities. */}}
{{- define "al.securityContext" -}}
allowPrivilegeEscalation: false
readOnlyRootFilesystem: true
runAsNonRoot: true
runAsUser: 65532
runAsGroup: 65532
capabilities:
  drop: [ALL]
seccompProfile:
  type: RuntimeDefault
{{- end -}}

{{/* Environment and mounts every collector needs to reach the server. */}}
{{- define "al.collectorEnv" -}}
- name: AL_INGEST_URL
  value: {{ required "collector.ingest.url is required when a collector is enabled" .Values.collector.ingest.url | quote }}
- name: AL_TOKEN_FILE
  value: /etc/audit-loupe/token/token
{{- if .Values.collector.ingest.caSecret }}
- name: AL_INGEST_CA_FILE
  value: /etc/audit-loupe/ca/ca.crt
{{- end }}
{{- end -}}

{{- define "al.collectorMounts" -}}
- name: token
  mountPath: /etc/audit-loupe/token
  readOnly: true
{{- if .Values.collector.ingest.caSecret }}
- name: ca
  mountPath: /etc/audit-loupe/ca
  readOnly: true
{{- end }}
{{- end -}}

{{- define "al.collectorVolumes" -}}
- name: token
  secret:
    secretName: {{ include "al.tokenSecret" . }}
    items: [{ key: token, path: token }]
{{- if .Values.collector.ingest.caSecret }}
- name: ca
  secret:
    secretName: {{ .Values.collector.ingest.caSecret }}
    items: [{ key: ca.crt, path: ca.crt }]
{{- end }}
{{- end -}}
