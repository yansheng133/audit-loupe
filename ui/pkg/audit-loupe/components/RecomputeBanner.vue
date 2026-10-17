<script>
import { getJson, postJson } from '../lib/api';
import { texts, fmtDate } from '../lib/model';

// Shown when the rule settings changed and stored records were judged by older ones. Asks
// before judging them again (it takes a while), shows progress, and tells the page to reload
// when done. "Not now" hides it in this browser until the settings change again.
const KEY = 'audit-loupe.recompute-dismissed';

export default {
  name: 'AuditLoupeRecomputeBanner',

  emits: ['done'],

  data() {
    return {
      T: texts(this.$store), status: null, msg: '', timer: null, dismissed: '', watching: false,
    };
  },

  computed: {
    R() {
      return this.T.recompute;
    },
    job() {
      return this.status?.job?.state === 'running' ? this.status.job : null;
    },
    pending() {
      const p = this.status?.pending;

      return p && this.dismissed !== this.status.fingerprint ? p : null;
    },
  },

  mounted() {
    try {
      this.dismissed = window.localStorage.getItem(KEY) || '';
    } catch (e) {}
    this.load();
  },

  beforeUnmount() {
    clearTimeout(this.timer);
  },

  methods: {
    fmtDate,
    async load() {
      try {
        this.status = await getJson('/api/rules');
      } catch (e) {
        // The page itself reports a missing backend.
        this.status = null;

        return;
      }
      if (this.job) {
        this.watching = true;
        this.timer = setTimeout(this.load, 2000);
      } else if (this.watching) {
        // Finished (possibly before the first poll).
        this.watching = false;
        if (this.status.job?.state === 'failed') {
          this.msg = this.R.failed(this.status.job.error);
        } else {
          this.msg = this.R.done;
          this.$emit('done');
        }
      }
    },
    async start() {
      this.msg = '';
      try {
        await postJson('/api/rules/recompute');
        this.watching = true;
      } catch (e) {
        this.msg = e.status === 403 ? this.R.forbidden : (e.status === 409 ? this.R.busy : this.R.failed(e.message));

        return;
      }
      await this.load();
    },
    later() {
      this.dismissed = this.status.fingerprint;
      try {
        window.localStorage.setItem(KEY, this.dismissed);
      } catch (e) {}
    },
  },
};
</script>

<template>
  <div
    v-if="job"
    class="banner-info"
    role="status"
  >
    <span>{{ R.running(job.done, job.total) }}</span>
    <progress
      :value="job.done"
      :max="job.total || 1"
    />
  </div>
  <div
    v-else-if="pending"
    class="banner-info"
    role="region"
    :aria-label="R.start"
  >
    <div class="banner-text">
      <strong>{{ R.pending(pending.events, fmtDate(status.sinceMs)) }}</strong>
      <span>{{ R.explain(pending.total, R.eta(pending.estimateSeconds)) }}</span>
      <span class="muted">{{ R.limits }}</span>
      <span
        v-if="msg"
        class="banner-msg"
      >{{ msg }}</span>
    </div>
    <div class="banner-actions">
      <button
        class="btn-al on"
        type="button"
        @click="start"
      >
        {{ R.start }}
      </button>
      <button
        class="btn-al"
        type="button"
        @click="later"
      >
        {{ R.later }}
      </button>
    </div>
  </div>
  <div
    v-else-if="msg"
    class="banner-info"
    role="status"
  >
    <span>{{ msg }}</span>
  </div>
</template>
