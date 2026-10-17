// Turns backend rows into the view model the change map draws: one entry per event, per
// object and per person, with the labels the mock design uses.

// ---------- text ----------
const ZH = {
  title: '變更地圖', crumb: '稽核', allClusters: '全部叢集', allProjects: '全部專案', allNs: '全部 namespace', everyone: '所有人',
  withSystem: '含系統元件', reset: '清除篩選', thisPeriod: '這段時間', changedObjects: '被動過的物件', evidence: '證據',
  evidenceHint: '誰、怎麼做的、改了什麼', added: '新增', modified: '修改', deleted: '刪除', readOnly: '只讀取／進入',
  clickForRelations: '點物件連出關係', ops: '筆操作', objChanged: '個物件變更', needAttention: '需要注意', items: '件',
  quiet: '這段時間沒有中、高風險的操作。', whoDid: '誰做了什麼', clickAgain: '再點一次取消', clickName: '點名字只看這個人',
  coverageHint: '叢集收集範圍標在中間每座叢集的標題列。', empty: '這段時間、這個範圍內沒有任何操作。放寬時間範圍或篩選條件試試。',
  pickTitle: '還沒選物件', pickBody: '從左邊的「需要注意」、中間的物件，或下方時間軸上的點，挑一個開始看。',
  opsToday: (n, dim) => `當天 ${ n } 筆操作${ dim ? '（範圍外的變淡）' : '' }`, whoDidIt: (t) => `${ t } 是誰做的`, whatChanged: (t) => `${ t } 改了什麼`,
  notes: (t) => `${ t } 補充`, related: '相關物件：', relatedLoading: '讀取相關物件…', hourly: '每小時量',
  rangeLabel: (a, b) => `範圍 A ${ a } → B ${ b }`, keys: '拖曳選範圍，A／B 可單獨拖動・●變更 ◆讀取或進入 ○刪除・紅圈為中高風險・灰底為非上班時間',
  allDay: '全天', workHours: (a, b) => `上班 ${ a }–${ b }`, afterHours: (b) => `下班後 ${ b }–24`, day: '日期', today: '今天',
  loading: '載入中…', backendMissing: '找不到 Audit Loupe 後端，或你沒有權限（需要後端所在叢集 audit-loupe-system namespace 的 services/proxy 權限）。',
  backendCluster: '後端所在叢集', backendSave: '套用', error: '讀取失敗', truncated: (n) => `只載入了前 ${ n } 筆，請縮小範圍`,
  cluster: '叢集', project: '專案', clusterScope: '叢集層級', notNamespaced: '不屬於 namespace', noProject: '不屬於專案',
  rancherSettings: 'Rancher 設定', rancherGlobal: 'Rancher 全域', projectPrefix: '專案 ', risk: (s) => `${ s }風險`,
  sev: { high: '高', medium: '中', low: '低' }, records: (n, who, t) => `${ n } 筆・最後 ${ who } ${ t }`,
  deletedUser: '已刪除', readded: (n) => `曾刪除後重新加入（${ n } 段）`, lifetime: (a, b) => `${ a } ～ ${ b || '現在' }`, split: '分開看', merge: '合併看',
  storyBurst: (who, m, n) => `${ who } 在 ${ m } 分鐘內連續 ${ n } 筆高權限操作`,
  coverage: '收集範圍：', covFull: '完整：apiserver audit ＋ 物件版本', covRancher: '經 Rancher 的操作', covObjects: '只知道改了什麼', covAudit: '只有 apiserver audit', covNone: '這段時間沒有資料',
  via: {
    ui: 'Rancher 介面', kubectl: 'Rancher kubectl', rancher: '經 Rancher', direct: '繞過 Rancher', ace: 'ACE 直連', imp: '其他代理身分', controller: '系統元件', unknown: '收集中斷'
  },
  verbs: {
    create: '新增', update: '修改', patch: '修改', delete: '刪除', deletecollection: '整批刪除', get: '讀取', list: '列出', login: '登入', generatekubeconfig: '產生 kubeconfig', exec: '進入容器（exec）', attach: '連進容器（attach）', portforward: '轉送連接埠'
  },
  refused: '，被拒絕', failed: (c) => `（失敗 ${ c }）`,
  rules: {
    namespace_deleted: '刪除 namespace', rbac_changed: '權限變更', pod_exec: '進入容器', secret_read: '讀取 Secret', bypassed_rancher: '繞過 Rancher',
    off_hours: '非上班時間', forbidden: '被拒絕', user_deleted: '刪除使用者', credential_issued: '發出憑證', login_failed: '登入失敗', unattributed: '無法歸屬'
  },
  chain: {
    idp: 'IdP 帳號', through: '經由', viaUi: 'Rancher 介面', viaKubectl: 'Rancher 的 kubeconfig', viaRancher: 'Rancher', impersonating: '代為執行', wrote: '寫入',
    rancherApi: 'Rancher API', apiserver: (c) => `${ c } apiserver`, credential: '憑證', component: '系統元件', who: '是誰', unknown: '不明', found: '發現方式', foundBy: '重新連線後比對物件版本',
    paired: '來源：Rancher audit log 與下游 apiserver audit log 對到同一筆，合併成一列。', rancherOnly: '來源：Rancher API audit log。',
    rancherOnlyDownstream: '只有 Rancher 的紀錄：這座叢集的 apiserver audit 沒有對到同一筆，可能沒開或還在路上。', downstreamOnly: '只有下游 apiserver 的紀錄，Rancher 那邊沒有對到。',
    direct: '只有下游 apiserver audit log，Rancher 那邊沒有紀錄，無法對應到個人。要追是誰，得查誰能取得這張憑證。',
    controller: 'Kubernetes 或 operator 自動產生的變更，通常起因是上游物件被修改。', unattributed: '只知道內容變了、大約在什麼時候，沒有 audit 紀錄可以對應。'
  },
  noteExec: '在容器裡下了哪些指令，audit log 不會記錄。', noteSecret: 'Secret 不保存版本，只記錄誰在何時存取。', noteConfigMap: 'ConfigMap 的值預設遮蔽，只顯示哪個鍵變了。',
  noteGone: '物件已不在叢集裡；顯示的是最後看到的內容。',
  rel: {
    owned_by: '屬於', mounts: '掛載', reads: '讀取', runs_as: '以此身分執行', pulls_with: '拉映像用', selects: '選到', applies_to: '套用到', routes_to: '導流到',
    uses_tls: '使用憑證', attached_to: '掛在', grants_role: '授予角色', grants_to: '授予給', scales: '自動調整', on: '範圍'
  },
};

const EN = {
  title: 'Change map', crumb: 'Audit', allClusters: 'All clusters', allProjects: 'All projects', allNs: 'All namespaces', everyone: 'Everyone',
  withSystem: 'Include system components', reset: 'Clear filters', thisPeriod: 'This period', changedObjects: 'Objects touched', evidence: 'Evidence',
  evidenceHint: 'Who, how, and what changed', added: 'Added', modified: 'Modified', deleted: 'Deleted', readOnly: 'Read or entered only',
  clickForRelations: 'Click an object to see its relations', ops: 'actions', objChanged: 'objects changed', needAttention: 'Needs attention', items: '',
  quiet: 'No medium or high risk actions in this period.', whoDid: 'Who did what', clickAgain: 'Click again to clear', clickName: 'Click a name to focus',
  coverageHint: 'Each cluster header shows what is collected there.', empty: 'Nothing happened in this range. Widen the time range or filters.',
  pickTitle: 'Nothing selected', pickBody: 'Pick an item under "Needs attention", an object in the middle, or a dot on the timeline.',
  opsToday: (n, dim) => `${ n } actions this day${ dim ? ' (outside the range dimmed)' : '' }`, whoDidIt: (t) => `Who did it at ${ t }`, whatChanged: (t) => `What changed at ${ t }`,
  notes: (t) => `Notes for ${ t }`, related: 'Related:', relatedLoading: 'Loading related objects…', hourly: 'Per hour',
  rangeLabel: (a, b) => `Range A ${ a } → B ${ b }`, keys: 'Drag to select; drag A or B alone · ● change ◆ read or enter ○ delete · red ring: medium/high risk · grey: outside working hours',
  allDay: 'All day', workHours: (a, b) => `Working ${ a }–${ b }`, afterHours: (b) => `After ${ b }`, day: 'Day', today: 'Today',
  loading: 'Loading…', backendMissing: 'The Audit Loupe backend was not found, or you lack access (services/proxy in the audit-loupe-system namespace of the backend cluster).',
  backendCluster: 'Backend cluster', backendSave: 'Apply', error: 'Failed to load', truncated: (n) => `Only the first ${ n } records were loaded; narrow the range`,
  cluster: 'Cluster', project: 'Project', clusterScope: 'Cluster scope', notNamespaced: 'Not namespaced', noProject: 'Not in a project',
  rancherSettings: 'Rancher settings', rancherGlobal: 'Rancher global', projectPrefix: 'Project ', risk: (s) => `${ s } risk`,
  sev: { high: 'High', medium: 'Medium', low: 'Low' }, records: (n, who, t) => `${ n } · last ${ who } ${ t }`,
  deletedUser: 'Deleted', readded: (n) => `Deleted and re-added (${ n } periods)`, lifetime: (a, b) => `${ a } – ${ b || 'now' }`, split: 'Split', merge: 'Merge',
  storyBurst: (who, m, n) => `${ who }: ${ n } privileged actions within ${ m } minutes`,
  coverage: 'Collected: ', covFull: 'Full: apiserver audit + object versions', covRancher: 'Actions through Rancher', covObjects: 'What changed only', covAudit: 'apiserver audit only', covNone: 'No data in this period',
  via: {
    ui: 'Rancher UI', kubectl: 'Rancher kubectl', rancher: 'Through Rancher', direct: 'Bypassed Rancher', ace: 'Authorized endpoint', imp: 'Other impersonation', controller: 'System component', unknown: 'Collection gap'
  },
  verbs: {
    create: 'created', update: 'updated', patch: 'patched', delete: 'deleted', deletecollection: 'deleted (collection)', get: 'read', list: 'listed', login: 'logged in', generatekubeconfig: 'generated a kubeconfig', exec: 'exec into container', attach: 'attached to container', portforward: 'port-forward'
  },
  refused: ', refused', failed: (c) => ` (failed ${ c })`,
  rules: {
    namespace_deleted: 'Namespace deleted', rbac_changed: 'Permissions changed', pod_exec: 'Exec into container', secret_read: 'Secret read', bypassed_rancher: 'Bypassed Rancher',
    off_hours: 'Outside working hours', forbidden: 'Refused', user_deleted: 'User deleted', credential_issued: 'Credential issued', login_failed: 'Login failed', unattributed: 'Unattributed'
  },
  chain: {
    idp: 'IdP account', through: 'Through', viaUi: 'Rancher UI', viaKubectl: 'Rancher kubeconfig', viaRancher: 'Rancher', impersonating: 'Acting as', wrote: 'Wrote to',
    rancherApi: 'Rancher API', apiserver: (c) => `${ c } apiserver`, credential: 'Credential', component: 'Component', who: 'Who', unknown: 'Unknown', found: 'Found by', foundBy: 'Comparing versions after reconnecting',
    paired: 'Source: the Rancher audit log and the downstream apiserver audit log matched as one action.', rancherOnly: 'Source: the Rancher API audit log.',
    rancherOnlyDownstream: 'Only Rancher recorded this: the cluster\'s apiserver audit did not match it (not enabled, or still on its way).', downstreamOnly: 'Only the downstream apiserver recorded this; nothing matched on the Rancher side.',
    direct: 'Only the downstream apiserver audit log has this; Rancher has no record, so no person can be named. Check who can obtain this credential.',
    controller: 'Made automatically by Kubernetes or an operator, usually because something upstream changed.', unattributed: 'Only what changed and roughly when are known; no audit record matches.'
  },
  noteExec: 'The commands typed inside the container are not in the audit log.', noteSecret: 'Secrets are not versioned; only who accessed them and when.', noteConfigMap: 'ConfigMap values are masked; only which key changed is shown.',
  noteGone: 'The object is gone; this is the last content seen.',
  rel: {
    owned_by: 'owned by', mounts: 'mounts', reads: 'reads', runs_as: 'runs as', pulls_with: 'pulls with', selects: 'selects', applies_to: 'applies to', routes_to: 'routes to',
    uses_tls: 'uses certificate', attached_to: 'attached to', grants_role: 'grants role', grants_to: 'grants to', scales: 'scales', on: 'scope'
  },
};

export function texts(store) {
  let locale = 'en-us';

  try {
    locale = store.getters['i18n/current']() || locale;
  } catch (e) {}

  return locale.startsWith('zh') ? ZH : EN;
}

// ---------- constants ----------
export const SEV_RANK = { high: 0, medium: 1, low: 2 };

export const RULE_SEV = {
  namespace_deleted: 'high', rbac_changed: 'medium', pod_exec: 'medium', bypassed_rancher: 'medium', user_deleted: 'medium',
  secret_read: 'low', off_hours: 'low', forbidden: 'low', credential_issued: 'low', login_failed: 'low', unattributed: 'low',
};

const KINDS = {
  deployments: 'Deployment', statefulsets: 'StatefulSet', daemonsets: 'DaemonSet', replicasets: 'ReplicaSet', cronjobs: 'CronJob', jobs: 'Job', pods: 'Pod',
  services: 'Service', configmaps: 'ConfigMap', secrets: 'Secret', namespaces: 'Namespace', serviceaccounts: 'ServiceAccount', persistentvolumeclaims: 'PersistentVolumeClaim',
  ingresses: 'Ingress', networkpolicies: 'NetworkPolicy', roles: 'Role', rolebindings: 'RoleBinding', clusterroles: 'ClusterRole', clusterrolebindings: 'ClusterRoleBinding',
  horizontalpodautoscalers: 'HorizontalPodAutoscaler', httproutes: 'HTTPRoute', nodes: 'Node',
  globalroles: 'GlobalRole', globalrolebindings: 'GlobalRoleBinding', roletemplates: 'RoleTemplate', clusterroletemplatebindings: 'ClusterRoleTemplateBinding',
  projectroletemplatebindings: 'ProjectRoleTemplateBinding', projects: 'Project', users: 'User', userattributes: 'UserAttribute', tokens: 'Token', clusters: 'Cluster', login: 'Login',
};

export const KIND_TAG = {
  Deployment: 'DEPLOY', StatefulSet: 'STS', DaemonSet: 'DS', ReplicaSet: 'RS', CronJob: 'CRON', Job: 'JOB', Pod: 'POD', Service: 'SVC', ConfigMap: 'CM', Secret: 'SECRET',
  Namespace: 'NS', ServiceAccount: 'SA', Ingress: 'ING', NetworkPolicy: 'NETPOL', Role: 'ROLE', RoleBinding: 'RB', ClusterRole: 'CR', ClusterRoleBinding: 'CRB',
  HorizontalPodAutoscaler: 'HPA', HTTPRoute: 'ROUTE', GlobalRole: 'GROLE', GlobalRoleBinding: 'GRB', RoleTemplate: 'RT', ClusterRoleTemplateBinding: 'CRTB',
  ProjectRoleTemplateBinding: 'PRTB', Project: 'PROJ', User: 'USER', Cluster: 'CLUSTER', Login: 'LOGIN', Token: 'TOKEN',
};

export const kindOf = (resource) => KINDS[resource] || resource;

// Distinct, legible in light and dark themes; system components are grey.
const PALETTE = ['#2f63d6', '#0f8b8d', '#8a4fd1', '#c2410c', '#4d7c0f', '#b4235a', '#0369a1', '#7c5e10'];
const GREY = '#8a8e9c';

// ---------- small helpers ----------
export const pad = (n) => String(n).padStart(2, '0');
export const fmtMin = (m) => (m >= 1440 ? '24:00' : `${ pad(Math.floor(m / 60)) }:${ pad(m % 60) }`);

export function dayStart(date) {
  const d = new Date(date);

  d.setHours(0, 0, 0, 0);

  return d;
}

export function fmtDate(ms) {
  const d = new Date(ms);

  return `${ d.getFullYear() }-${ pad(d.getMonth() + 1) }-${ pad(d.getDate()) } ${ pad(d.getHours()) }:${ pad(d.getMinutes()) }`;
}

export const sevOf = (ev) => ev.alerts.map((r) => RULE_SEV[r] || 'low').sort((x, y) => SEV_RANK[x] - SEV_RANK[y])[0] || null;
export const worst = (list) => list.filter(Boolean).sort((x, y) => SEV_RANK[x] - SEV_RANK[y])[0] || null;
export const isRisky = (ev) => ['high', 'medium'].includes(sevOf(ev));

export function netState(evs) {
  const ch = evs.filter((e) => e.change).sort((x, y) => x.t - y.t);

  if (!ch.length) {
    return 'none';
  }
  const last = ch[ch.length - 1].change;
  const first = ch[0].change;

  if (last === 'deleted') {
    return first === 'added' ? 'none' : 'deleted';
  }

  return first === 'added' ? 'added' : 'modified';
}

const short = (v) => {
  if (v === undefined) {
    return '';
  }
  const s = typeof v === 'string' ? v : JSON.stringify(v);

  return s.length > 90 ? `${ s.slice(0, 87) }…` : s;
};

// "openldap_user://uid=alice,ou=people,dc=example" -> "uid=alice"
export function principalShort(p) {
  if (!p) {
    return '';
  }
  const rest = p.includes('://') ? p.split('://')[1] : p;

  return rest.split(',')[0];
}

function viaOf(row) {
  if (row.unattributed) {
    return 'unknown';
  }
  if (!row.human) {
    return 'controller';
  }
  // Rancher's proxy passes the client's User-Agent on, so the downstream record has it too.
  const ua = (row.rancher?.user_agent || row.user_agent || '').toLowerCase();

  if (row.source === 'rancher' || row.via === 'rancher') {
    if (ua.includes('kubectl')) {
      return 'kubectl';
    }
    if (ua.includes('mozilla')) {
      return 'ui';
    }
    // Steve impersonates through the cluster agent: that is the Rancher UI.
    if (row.caller === 'system:serviceaccount:cattle-system:cattle') {
      return 'ui';
    }

    return 'rancher';
  }

  return { direct: 'direct', ace: 'ace' }[row.via] || 'imp';
}

function changeOf(row) {
  const c = row.change;

  if (c) {
    if (c.startsWith('added')) {
      return 'added';
    }
    if (c === 'deleted' || c === 'deleting') {
      return 'deleted';
    }

    return 'modified';
  }
  if (row.code >= 400) {
    return null;
  }

  return { create: 'added', update: 'modified', patch: 'modified', delete: 'deleted', deletecollection: 'deleted' }[row.verb] || null;
}

function diffLines(row) {
  const out = [];

  for (const c of row.changes || []) {
    if (out.length >= 40) {
      out.push([' ', '…']);
      break;
    }
    if (c.op !== 'added') {
      out.push(['-', `${ c.path }: ${ short(c.old) }`]);
    }
    if (c.op !== 'removed') {
      out.push(['+', `${ c.path }: ${ short(c.new) }`]);
    }
  }
  if (!out.length && row.body) {
    const sign = row.change === 'deleted' ? '-' : '+';
    const walk = (prefix, v) => {
      if (out.length >= 14 || v === undefined) {
        return;
      }
      if (v && typeof v === 'object' && !Array.isArray(v)) {
        Object.entries(v).forEach(([k, x]) => walk(prefix ? `${ prefix }.${ k }` : k, x));
      } else {
        out.push([sign, `${ prefix }: ${ short(v) }`]);
      }
    };

    walk('kind', row.body.kind);
    walk('metadata.name', row.body.metadata?.name);
    ['spec', 'data', 'roleRef', 'subjects', 'roleTemplateName', 'globalRoleName', 'userName', 'groupPrincipalName', 'projectName', 'clusterName'].forEach((k) => {
      if (row.body[k] !== undefined) {
        walk(k, row.body[k]);
      }
    });
  }

  return out;
}

// ---------- the view model ----------
// rows: /api/timeline, unattributed: /api/unattributed, actors: /api/actors,
// nsProjects: [{cluster, namespace, projectId}], projectName(cluster, id) and
// clusterName(id) come from Rancher's own store.
export function buildModel({
  rows, unattributed, actors, nsProjects, projectName, clusterName, day, T,
}) {
  const start = dayStart(day).getTime();
  const nsProject = new Map(nsProjects.map((p) => [`${ p.cluster }|${ p.namespace }`, p.projectId]));
  const OBJ = {};
  const ACTORS = {};
  const EVENTS = [];
  let color = 0;

  const actorFor = (key, label, human, info) => {
    if (!ACTORS[key]) {
      const a = actors.find((x) => x.key === key);

      ACTORS[key] = {
        key,
        label:     a?.actor || label,
        role:      info,
        color:     human ? PALETTE[color++ % PALETTE.length] : GREY,
        system:    !human,
        userId:    a?.user_id || '',
        deleted:   !!a?.deleted,
        lifetimes: a?.lifetimes || [],
      };
    }

    return key;
  };

  const objFor = (cluster, group, resource, ns, name) => {
    const kind = kindOf(resource);
    const key = `${ cluster }|${ kind }|${ ns }|${ name }`;

    if (!OBJ[key]) {
      let project; let nsLabel;

      if (group === 'management.cattle.io' || resource === 'login') {
        project = T.rancherSettings;
        nsLabel = T.rancherGlobal;
      } else if (!ns) {
        project = T.clusterScope;
        nsLabel = T.notNamespaced;
      } else {
        const pid = nsProject.get(`${ cluster }|${ ns }`);

        project = pid ? (projectName(cluster, pid) || pid) : T.noProject;
        nsLabel = ns;
      }
      OBJ[key] = {
        key, cluster, clusterLabel: clusterName(cluster) || cluster, project, ns: nsLabel, namespace: ns, kind, name, group, resource,
      };
    }

    return key;
  };

  for (const r of rows) {
    const ts = new Date(r.ts).getTime();
    const t = Math.round((ts - start) / 60000);

    if (t < 0 || t > 1440) {
      continue;
    }
    const via = viaOf(r);
    const key = r.user_id || r.actor;
    const actor = actorFor(key, r.actor, r.human, r.human ? principalShort(r.principal) : r.caller);
    const obj = objFor(r.cluster, r.api_group, r.subresource && r.resource === 'pods' ? 'pods' : r.resource, r.namespace, r.name || (r.resource === 'login' ? '' : '(collection)'));
    const verb = T.verbs[r.subresource] || T.verbs[r.verb] || r.verb;
    const alerts = [...new Set(r.alerts || [])];
    const notes = [];

    if (['exec', 'attach'].includes(r.subresource)) {
      notes.push(T.noteExec);
    }
    if (r.resource === 'secrets') {
      notes.push(T.noteSecret);
    }
    if (r.resource === 'configmaps' && r.changes?.length) {
      notes.push(T.noteConfigMap);
    }
    if (r.change === 'deleted') {
      notes.push(T.noteGone);
    }
    EVENTS.push({
      id:     `e${ r.id }`,
      t,
      ts,
      hhmm:   fmtMin(Math.max(0, Math.min(1439, t))),
      actor,
      via,
      verb:   `${ verb }${ r.code === 403 ? T.refused : (r.code >= 400 ? T.failed(r.code) : '') }`,
      obj,
      change: changeOf(r),
      alerts,
      diff:   diffLines(r),
      note:   notes.join(' '),
      raw:    r,
    });
  }

  for (const u of unattributed) {
    const ts = new Date(u.observed).getTime();
    const t = Math.round((ts - start) / 60000);

    if (t < 0 || t > 1440) {
      continue;
    }
    const actor = actorFor('__unknown', T.chain.unknown, true, T.via.unknown);

    ACTORS[actor].color = GREY;
    const row = { ...u, unattributed: true };

    EVENTS.push({
      id:     `u${ u.id }`,
      t,
      ts,
      hhmm:   fmtMin(Math.max(0, Math.min(1439, t))),
      actor,
      via:    'unknown',
      verb:   T.verbs[u.change === 'deleted' ? 'delete' : (u.change.startsWith('added') ? 'create' : 'update')],
      obj:    objFor(u.cluster, u.api_group, u.resource, u.namespace, u.name),
      change: changeOf(row),
      alerts: ['unattributed'],
      diff:   diffLines(row),
      note:   '',
      raw:    row,
    });
  }
  EVENTS.sort((a, b) => a.ts - b.ts);

  return { OBJ, ACTORS, EVENTS };
}

// How the action reached the cluster, as hops for the evidence panel.
export function identityChain(ev, obj, actor, T) {
  const r = ev.raw;
  const C = T.chain;
  const idp = principalShort(r.principal) || actor.label;

  if (ev.via === 'unknown') {
    return { hops: [[C.who, C.unknown, true], [C.found, C.foundBy]], note: C.unattributed };
  }
  if (ev.via === 'controller') {
    return { hops: [[C.component, r.caller], [C.wrote, C.apiserver(obj.clusterLabel)]], note: C.controller };
  }
  if (ev.via === 'direct') {
    return { hops: [[C.credential, r.caller, true], [C.wrote, C.apiserver(obj.clusterLabel)]], note: C.direct };
  }
  const through = { ui: C.viaUi, kubectl: C.viaKubectl }[ev.via] || C.viaRancher;
  const hops = [[C.idp, idp], [C.through, through]];

  if (r.source === 'rancher') {
    if (!r.cluster || r.cluster === 'local' || obj.group === 'management.cattle.io') {
      hops.push([C.wrote, C.rancherApi]);

      return { hops, note: C.rancherOnly };
    }
    hops.push([C.wrote, C.apiserver(obj.clusterLabel)]);

    return { hops, note: C.rancherOnlyDownstream };
  }
  if (r.caller && r.caller !== r.user_id) {
    hops.push([C.impersonating, r.caller.split(':').pop()]);
  }
  hops.push([C.wrote, C.apiserver(obj.clusterLabel)]);

  return { hops, note: r.rancher ? C.paired : (ev.via === 'ace' || ev.via === 'imp' ? '' : C.downstreamOnly) };
}

// Coverage label for a cluster from what it sent in the period.
export function coverageOf(row, T) {
  if (!row) {
    return { label: T.covNone, tone: 'none' };
  }
  const audit = row.k8s_events > 0;
  const objects = row.versions > 0 || !!row.last_version;

  if (audit && objects) {
    return { label: T.covFull, tone: 'added' };
  }
  if (audit) {
    return { label: T.covAudit, tone: 'modified' };
  }
  if (row.rancher_events > 0) {
    return { label: T.covRancher, tone: 'modified' };
  }
  if (objects) {
    return { label: T.covObjects, tone: 'modified' };
  }

  return { label: T.covNone, tone: 'none' };
}
