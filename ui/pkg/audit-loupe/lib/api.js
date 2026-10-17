// The backend runs inside each downstream cluster. The UI reaches it through the
// Rancher service proxy, so Rancher login and RBAC apply: a user needs "get" on
// services/proxy in the backend namespace to see anything.

export const BACKEND = {
  namespace: 'audit-loupe-system',
  service:   'audit-loupe',
  port:      8080,
};

export function baseUrl(cluster) {
  const { namespace, service, port } = BACKEND;

  return `/k8s/clusters/${ encodeURIComponent(cluster) }/api/v1/namespaces/${ namespace }/services/http:${ service }:${ port }/proxy`;
}

export async function getJson(cluster, path, params = {}) {
  const qs = new URLSearchParams(Object.entries(params).filter(([, v]) => v !== undefined && v !== null && v !== '')).toString();
  const res = await fetch(`${ baseUrl(cluster) }${ path }${ qs ? `?${ qs }` : '' }`, {
    credentials: 'same-origin',
    headers:     { Accept: 'application/json' },
  });

  if (!res.ok) {
    const err = new Error(`HTTP ${ res.status }`);

    err.status = res.status;
    throw err;
  }

  return res.json();
}

const ZH = {
  timeline:        '操作時間線',
  objects:         '物件變更史',
  alerts:          '異常提示',
  actor:           '帳號',
  allActors:       '全部帳號',
  range:           '時間範圍',
  h24:             '最近 24 小時',
  h72:             '最近 3 天',
  h168:            '最近 7 天',
  h720:            '最近 30 天',
  showAll:         '包含系統元件與讀取動作',
  time:            '時間',
  via:             '經由',
  action:          '動作',
  object:          '物件',
  result:          '結果',
  changes:         '變更內容',
  flags:           '提示',
  noData:          '這段時間沒有紀錄',
  loading:         '載入中…',
  search:          '搜尋 namespace、名稱或類型',
  versions:        '版本數',
  lastChange:      '最後變更',
  history:         '變更歷程',
  pickObject:      '從左邊選一個物件',
  unattributed:    '找不到對應的 audit 紀錄',
  unseen:          '工具未運作期間的變更，對不上 audit 紀錄',
  baseline:        '開始追蹤時的狀態',
  added:           '建立',
  modified:        '修改',
  deleted:         '刪除',
  showSpec:        '顯示完整內容',
  hideSpec:        '收起',
  backendMissing:  '這個叢集還沒有部署 audit-loupe，或你沒有存取權限（需要 audit-loupe-system namespace 的 services/proxy 權限）。',
  error:           '讀取失敗',
  requestBody:     '送出的內容',
  severity:        '嚴重度',
  rule:            '規則',
  viaRancher:      'Rancher',
  viaDirect:       '直連 API',
  viaImpersonation: '代理身分',
  viaAce:          'ACE 直連',
  valueChanged:    '值已變更',
  valueHidden:     '內容不保存',
  addedUnseen:     '建立（工具未運作期間）',
  deleting:        '刪除中',
  viewHistory:     '看這個物件的變更史',
  rules:           {
    namespace_deleted: '刪除 namespace',
    rbac_changed:      '變更權限設定（RBAC）',
    pod_exec:          '進入容器執行指令',
    secret_read:       '讀取 Secret',
    bypassed_rancher:  '未經 Rancher，直接連 API server',
    off_hours:         '非上班時間變更',
    forbidden:         '權限不足被拒',
  },
  sev: {
    high: '高', medium: '中', low: '低'
  },
};

const EN = {
  timeline:        'Activity timeline',
  objects:         'Object history',
  alerts:          'Alerts',
  actor:           'User',
  allActors:       'All users',
  range:           'Range',
  h24:             'Last 24 hours',
  h72:             'Last 3 days',
  h168:            'Last 7 days',
  h720:            'Last 30 days',
  showAll:         'Include system components and reads',
  time:            'Time',
  via:             'Via',
  action:          'Action',
  object:          'Object',
  result:          'Result',
  changes:         'Changes',
  flags:           'Flags',
  noData:          'No records in this range',
  loading:         'Loading…',
  search:          'Search namespace, name or kind',
  versions:        'Versions',
  lastChange:      'Last change',
  history:         'History',
  pickObject:      'Pick an object on the left',
  unattributed:    'No matching audit record',
  unseen:          'Changed while not tracking; no matching audit record',
  baseline:        'State when tracking started',
  added:           'Created',
  modified:        'Modified',
  deleted:         'Deleted',
  showSpec:        'Show full object',
  hideSpec:        'Hide',
  backendMissing:  'audit-loupe is not deployed in this cluster, or you lack access (services/proxy in the audit-loupe-system namespace).',
  error:           'Failed to load',
  requestBody:     'Request body',
  severity:        'Severity',
  rule:            'Rule',
  viaRancher:      'Rancher',
  viaDirect:       'Direct API',
  viaImpersonation: 'Impersonation',
  viaAce:          'Authorized endpoint',
  valueChanged:    'value changed',
  valueHidden:     'content not stored',
  addedUnseen:     'Created (while not tracking)',
  deleting:        'Deleting',
  viewHistory:     'Object history',
  rules:           {
    namespace_deleted: 'Namespace deleted',
    rbac_changed:      'RBAC changed',
    pod_exec:          'Exec into container',
    secret_read:       'Secret read',
    bypassed_rancher:  'Bypassed Rancher (direct API access)',
    off_hours:         'Change outside working hours',
    forbidden:         'Forbidden request',
  },
  sev: {
    high: 'High', medium: 'Medium', low: 'Low'
  },
};

export function dict(store) {
  let locale = 'en-us';

  try {
    locale = store.getters['i18n/current']() || locale;
  } catch (e) {}

  return locale.startsWith('zh') ? ZH : EN;
}

export function fmtTime(iso) {
  if (!iso) {
    return '';
  }

  return new Date(iso).toLocaleString(undefined, {
    year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false
  });
}

export function fmtValue(v) {
  if (v === undefined) {
    return '';
  }
  if (v === null) {
    return 'null';
  }
  const s = typeof v === 'string' ? v : JSON.stringify(v);

  return s.length > 120 ? `${ s.slice(0, 117) }…` : s;
}

export function objLabel(e) {
  const kind = e.subresource ? `${ e.resource }/${ e.subresource }` : e.resource;
  const where = e.namespace ? `${ e.namespace }/` : '';

  return `${ kind } ${ where }${ e.name || '' }`.trim();
}
