/** @typedef {"debug"|"info"|"warn"|"error"|"fatal"} Level */
/** @typedef {"idle"|"sending"|"ok"|"failed"|"unable"} Status */

/**
 * @typedef {Object} Entry
 * @property {string} app
 * @property {Level} level
 * @property {string} message
 * @property {string} [source]
 * @property {string} [timestamp]
 * @property {Record<string, unknown>} [meta]
 */

export class TinyLogClient {
  /**
   * @param {{
   *   baseUrl: string,
   *   apiKey: string,
   *   app: string,
   *   source?: string,
   *   maxRetries?: number,
   *   fetchImpl?: typeof fetch,
   *   onUnable?: (err: Error, entries: Entry[]) => void,
   * }} opts
   */
  constructor(opts) {
    this.baseUrl = String(opts.baseUrl || "").replace(/\/$/, "");
    this.apiKey = opts.apiKey;
    this.app = opts.app;
    this.source = opts.source || "node";
    this.maxRetries = opts.maxRetries ?? 3;
    this.fetchImpl = opts.fetchImpl || globalThis.fetch.bind(globalThis);
    this.onUnable = opts.onUnable || null;
    /** @type {Status} */
    this.status = "idle";
    /** @type {string|null} */
    this.lastError = null;
    this._lock = Promise.resolve();
  }

  /**
   * @param {Level} level
   * @param {string} message
   * @param {Record<string, unknown>} [meta]
   */
  async log(level, message, meta) {
    return this.send({
      app: this.app,
      level,
      message,
      source: this.source,
      timestamp: new Date().toISOString(),
      meta,
    });
  }

  /** @param {Entry} entry */
  async send(entry) {
    return this.sendBatch([entry]);
  }

  /** @param {Entry[]} entries */
  async sendBatch(entries) {
    const run = async () => {
      if (!entries.length) return;
      const normalized = entries.map((e) => ({
        app: e.app || this.app,
        level: String(e.level || "info").toLowerCase(),
        message: e.message,
        source: e.source || this.source,
        timestamp: e.timestamp || new Date().toISOString(),
        meta: e.meta,
      }));

      this.status = "sending";
      this.lastError = null;
      let lastErr = null;

      for (let attempt = 0; attempt < this.maxRetries; attempt++) {
        try {
          await this._post(normalized);
          this.status = "ok";
          return;
        } catch (err) {
          lastErr = err instanceof Error ? err : new Error(String(err));
          if (isNonRetryable(lastErr)) break;
          await sleep(200 * 2 ** attempt);
        }
      }

      this.status = "unable";
      this.lastError = lastErr ? lastErr.message : "unable to send log";
      if (this.onUnable) this.onUnable(lastErr || new Error(this.lastError), normalized);
      throw lastErr || new Error(this.lastError);
    };

    const next = this._lock.then(run, run);
    this._lock = next.catch(() => {});
    return next;
  }

  /** @param {Entry[]} entries */
  async _post(entries) {
    const single = entries.length === 1;
    const url = single
      ? `${this.baseUrl}/api/v1/logs`
      : `${this.baseUrl}/api/v1/logs/batch`;
    const body = single ? entries[0] : { logs: entries };
    const res = await this.fetchImpl(url, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${this.apiKey}`,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(body),
    });
    if (res.ok) return;
    const text = await res.text().catch(() => "");
    const err = new Error(`tiny-log http ${res.status}: ${text}`);
    err.status = res.status;
    throw err;
  }
}

function isNonRetryable(err) {
  const s = err && err.status;
  return s === 400 || s === 401 || s === 403 || s === 413;
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}
