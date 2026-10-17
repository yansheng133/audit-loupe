{{- define "at.labels" -}}
app.kubernetes.io/name: audit-loupe
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}
