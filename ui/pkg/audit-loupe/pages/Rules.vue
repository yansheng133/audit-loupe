<script>
import RecomputeBanner from '../components/RecomputeBanner.vue';
import { getJson } from '../lib/api';
import { texts, fmtDate } from '../lib/model';

// The rule settings in force: on or off, severity (and the default), exempt scopes with their
// reasons; earlier settings and re-judging runs. Read-only: settings come from the Helm values.
export default {
  name: 'AuditLoupeRules',

  components: { RecomputeBanner },

  data() {
    return {
      T: texts(this.$store), status: null, loading: false, error: null, now: Date.now(),
    };
  },

  computed: {
    P() {
      return this.T.rulesPage;
    },
  },

  mounted() {
    this.load();
  },

  methods: {
    fmtDate,
    async load() {
      this.loading = true;
      this.error = null;
      this.now = Date.now();
      try {
        this.status = await getJson('/api/rules');
      } catch (e) {
        this.error = [403, 404, 503].includes(e.status) ? this.T.backendMissing : `${ this.T.error }: ${ e.message }`;
      } finally {
        this.loading = false;
      }
    },
    sevLabel(s) {
      return this.T.sev[s] || s;
    },
    scopes(x) {
      return Object.keys(this.P.scope)
        .filter((k) => k === 'namespaceLabels' ? x[k] && Object.keys(x[k]).length : x[k]?.length)
        .map((k) => [this.P.scope[k], k === 'namespaceLabels' ? Object.entries(x[k]).map(([a, b]) => `${ a }=${ b }`).join(', ') : x[k].join(', ')]);
    },
    expired(x) {
      // The backend reads a bare date as the end of that day in its time zone; close enough here.
      return x.until && new Date(x.until.length === 10 ? `${ x.until }T23:59:59` : x.until).getTime() < this.now;
    },
    // What a past settings entry changed from the built-in defaults.
    summary(settings) {
      const defaults = Object.fromEntries((this.status?.rules || []).map((r) => [r.id, r.defaultSeverity]));
      const out = [];

      for (const r of settings?.rules || []) {
        const parts = [];

        if (!r.enabled) {
          parts.push(this.P.off);
        } else {
          if (defaults[r.id] && r.severity !== defaults[r.id]) {
            parts.push(this.sevLabel(r.severity));
          }
          if (r.except?.length) {
            parts.push(`${ this.P.exceptions } ${ r.except.length }`);
          }
        }
        if (parts.length) {
          out.push(`${ this.T.rules[r.id] || r.id }${ this.P.sep[0] }${ parts.join(this.P.sep[1]) }`);
        }
      }
      if (settings?.changeWindows?.length) {
        out.push(this.P.windows(settings.changeWindows.length));
      }

      return out.length ? out.join(this.P.sep[2]) : this.P.same;
    },
  },
};
</script>

<template>
  <div class="al al-page al-rules">
    <div class="topbar">
      <span class="crumb">Audit Loupe ／ {{ T.crumb }}</span>
      <h1>{{ P.title }}</h1>
      <div class="filters">
        <button
          class="btn-al"
          type="button"
          @click="load"
        >
          {{ P.refresh }}
        </button>
      </div>
    </div>
    <RecomputeBanner
      ref="banner"
      @done="load"
    />
    <div
      v-if="error"
      class="banner-al"
    >
      {{ error }}
    </div>
    <div
      v-if="loading && !status"
      class="quiet"
    >
      {{ T.loading }}
    </div>

    <template v-if="status">
      <section class="col">
        <div class="col-head">
          <h2>{{ P.title }}</h2>
          <span class="muted">{{ P.code }} {{ status.fingerprint }}・{{ P.since }} {{ fmtDate(status.sinceMs) }}</span>
        </div>
        <div class="detail">
          <p class="muted">
            {{ P.intro }}
          </p>
          <p class="muted">
            {{ P.windows(status.changeWindows) }}
          </p>
          <p
            v-if="!status.pending && status.job?.state !== 'running'"
            class="muted"
          >
            {{ P.forceHint }}
            <button
              class="btn-al"
              type="button"
              @click="$refs.banner.start(true)"
            >
              {{ P.force }}
            </button>
          </p>
          <table class="tbl">
            <thead>
              <tr>
                <th>{{ P.rule }}</th><th>{{ P.what }}</th><th>{{ P.state }}</th><th>{{ P.severity }}</th><th>{{ P.exceptions }}</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="r in status.rules"
                :key="r.id"
                :class="{ dim: !r.enabled }"
              >
                <td>
                  <strong>{{ T.rules[r.id] || r.id }}</strong>
                  <div class="muted mono">
                    {{ r.id }}
                  </div>
                </td>
                <td>{{ P.what_[r.id] }}</td>
                <td>
                  <span
                    class="pill"
                    :class="r.enabled ? 'added' : 'none'"
                  >{{ r.enabled ? P.on : P.off }}</span>
                </td>
                <td>
                  <span
                    class="pill"
                    :class="r.severity"
                  >{{ sevLabel(r.severity) }}</span>
                  <div
                    v-if="r.severity !== r.defaultSeverity"
                    class="muted"
                  >
                    {{ P.defaultWas(sevLabel(r.defaultSeverity)) }}
                  </div>
                </td>
                <td>
                  <span v-if="!r.except.length">{{ P.none }}</span>
                  <ul
                    v-else
                    class="exc"
                  >
                    <li
                      v-for="(x, i) in r.except"
                      :key="i"
                      :class="{ dim: expired(x) }"
                    >
                      <span
                        v-for="[k, v] in scopes(x)"
                        :key="k"
                      >{{ k }}{{ P.sep[0] }}<code>{{ v }}</code>　</span>
                      <div>{{ P.reason }}{{ P.sep[0] }}{{ x.reason }}</div>
                      <div
                        v-if="x.until"
                        class="muted"
                      >
                        {{ P.until(x.until) }}<span v-if="expired(x)"> ({{ P.expired }})</span>
                      </div>
                    </li>
                  </ul>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <section class="col">
        <div class="col-head">
          <h2>{{ P.history }}</h2>
        </div>
        <div class="detail">
          <table class="tbl">
            <thead>
              <tr>
                <th>{{ P.since }}</th><th>{{ P.code }}</th><th>{{ P.summary }}</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="h in status.history"
                :key="h.id"
              >
                <td class="num">
                  {{ fmtDate(h.activeFromMs) }}
                </td>
                <td class="mono">
                  {{ h.fingerprint }}<span
                    v-if="h.fingerprint === status.fingerprint"
                    class="muted"
                  >（{{ P.current }}）</span>
                </td>
                <td>{{ summary(h.settings) }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <section
        v-if="status.recomputes.length"
        class="col"
      >
        <div class="col-head">
          <h2>{{ P.recomputes }}</h2>
        </div>
        <div class="detail">
          <table class="tbl">
            <thead>
              <tr>
                <th>{{ P.ran }}</th><th>{{ P.code }}</th><th>{{ P.covered }}</th><th>{{ P.records }}</th><th>{{ P.result }}</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="r in status.recomputes"
                :key="r.id"
              >
                <td class="num">
                  {{ fmtDate(r.startedMs) }}
                </td>
                <td class="mono">
                  {{ (status.history.find((h) => h.id === r.settingsId) || {}).fingerprint || r.settingsId }}
                </td>
                <td class="num">
                  <template v-if="r.fromMs">
                    {{ fmtDate(r.fromMs) }} – {{ fmtDate(r.toMs) }}
                  </template>
                </td>
                <td class="num">
                  {{ r.events }}
                </td>
                <td>{{ r.error ? T.recompute.failed(r.error) : (r.finishedMs ? P.ok : P.inProgress) }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>
    </template>
  </div>
</template>

<style lang="scss">
@import '../styles/change-map.scss';
.al.al-rules { height: auto; display: flex; flex-direction: column; }
.al.al-rules .col { flex: none; }
.al-rules .dim { opacity: .55; }
.al-rules .mono { font-family: var(--al-mono); font-size: 12px; }
.al-rules .exc { margin: 0; padding-left: 16px; }
.al-rules .exc li + li { margin-top: 6px; }
.al-rules .tbl td { vertical-align: top; }
</style>
