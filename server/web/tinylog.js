(function (root, factory) {
  if (typeof module === "object" && module.exports) module.exports = factory();
  else root.TinyLogBrowser = factory();
})(typeof self !== "undefined" ? self : this, function () {
  "use strict";

  function TinyLogBrowser(opts) {
    this.baseUrl = String(opts.baseUrl || "").replace(/\/$/, "");
    this.clientToken = opts.clientToken;
    this.app = opts.app;
    this.source = opts.source || "browser";
    this.maxRetries = opts.maxRetries == null ? 3 : opts.maxRetries;
    this.onUnable = opts.onUnable || null;
    this.status = "idle";
    this.lastError = null;
    this._queue = [];
    this._flushing = false;
  }

  TinyLogBrowser.prototype.log = function (level, message, meta) {
    return this.send({
      app: this.app,
      level: level,
      message: message,
      source: this.source,
      timestamp: new Date().toISOString(),
      meta: meta,
    });
  };

  TinyLogBrowser.prototype.send = function (entry) {
    var self = this;
    return new Promise(function (resolve, reject) {
      self._queue.push({ entry: entry, resolve: resolve, reject: reject });
      self._pump();
    });
  };

  TinyLogBrowser.prototype._pump = function () {
    if (this._flushing) return;
    var item = this._queue.shift();
    if (!item) return;
    this._flushing = true;
    var self = this;
    this._deliver(item.entry)
      .then(function () {
        item.resolve();
      })
      .catch(function (err) {
        item.reject(err);
      })
      .then(function () {
        self._flushing = false;
        self._pump();
      });
  };

  TinyLogBrowser.prototype._deliver = function (entry) {
    var self = this;
    var payload = {
      app: entry.app || this.app,
      level: String(entry.level || "info").toLowerCase(),
      message: entry.message,
      source: entry.source || this.source,
      timestamp: entry.timestamp || new Date().toISOString(),
      meta: entry.meta,
    };

    this.status = "sending";
    this.lastError = null;

    function attempt(n) {
      return self._post(payload).then(
        function () {
          self.status = "ok";
        },
        function (err) {
          var nonRetry =
            err &&
            (err.status === 400 ||
              err.status === 401 ||
              err.status === 403 ||
              err.status === 413);
          if (n + 1 >= self.maxRetries || nonRetry) {
            self.status = "unable";
            self.lastError = err && err.message ? err.message : "unable to send log";
            if (self.onUnable) self.onUnable(err, [payload]);
            return Promise.reject(err);
          }
          return sleep(200 * Math.pow(2, n)).then(function () {
            return attempt(n + 1);
          });
        }
      );
    }

    return attempt(0);
  };

  TinyLogBrowser.prototype._post = function (payload) {
    return fetch(this.baseUrl + "/api/v1/client/logs", {
      method: "POST",
      headers: {
        Authorization: "Bearer " + this.clientToken,
        "Content-Type": "application/json",
      },
      body: JSON.stringify(payload),
      credentials: "omit",
      keepalive: true,
    }).then(function (res) {
      if (res.ok) return;
      return res.text().then(function (text) {
        var err = new Error("tiny-log http " + res.status + ": " + text);
        err.status = res.status;
        throw err;
      });
    });
  };

  function sleep(ms) {
    return new Promise(function (r) {
      setTimeout(r, ms);
    });
  }

  return TinyLogBrowser;
});
