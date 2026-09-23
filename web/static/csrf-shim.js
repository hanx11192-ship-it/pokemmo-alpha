// ============================================================================
// CSRF 头注入垫片
// ============================================================================
//
// # 为什么需要这个东西
//
// Rust 版后端要求「所有状态变更请求」带上自定义头
//
//     x-requested-with: pokemmo-alpha-panel
//
// 它是 CSRF 防护的第二道防线（第一道是 cookie 的 `SameSite=Lax`）：
// 跨站请求能自动带上 cookie，但**没法自动带自定义头** —— 那会触发 CORS
// 预检，被同源策略挡下。
//
// 问题是原版前端**一个这种头都没发**（`panel/static/` 下 grep 零命中），
// 而原版后端也压根没检查过。所以我们收紧校验之后，如果什么都不做，
// 面板上每一个按钮都会 400：保存配置、上传插件、切换开关、推送测试……
//
// # 为什么用包装而不是改 app.js
//
// 改 app.js 是最直接的做法，但它有 1215 行、几十个调用点，
// 每加一个写接口就得记得补一次 —— 这是个会持续退化的约定。
// 在这里集中包一层，前端业务代码一行不用动，而且**新加的接口自动覆盖**。
//
// 用户对前端的原话是「就没必要重构了，现在用着还行」——
// 这个垫片正是让那句话成立的东西。
//
// # 为什么同时包 fetch 和 XHR
//
// 因为原版两种都在用：普通接口走 `fetch`，但文件上传（插件、适配器）
// 走的是 `XMLHttpRequest`（为了拿上传进度）。只包 fetch 的话，
// 上传功能会在部署后直接坏掉，而且是个很隐蔽的坏法 ——
// 点「上传」没反应，控制台里一个 400。
//
// 另外还兜了 `navigator.sendBeacon`（登出可能用），以及动态插入的
// `<form>` 提交路径没有覆盖 —— 原版没有这种用法，真出现了再说。

(function () {
  "use strict";

  var CSRF_HEADER = "x-requested-with";
  var CSRF_VALUE = "pokemmo-alpha-panel";

  // 只对**同源**请求补头。
  // 给跨域请求加自定义头会把一个简单请求变成预检请求，
  // 有可能把原本能用的跨域调用弄坏；而这本来也不是我们该管的请求。
  function isSameOrigin(url) {
    try {
      var u = new URL(url, window.location.href);
      return u.origin === window.location.origin;
    } catch (e) {
      // 相对路径等情况解析失败时，按同源处理（它就是同源）
      return true;
    }
  }

  // 只对状态变更方法补头。给 GET 也加上没坏处，但会让日志和抓包更难读，
  // 而且万一将来后端对 GET 也做校验，这里的意图就不清楚了。
  function isStateChanging(method) {
    var m = String(method || "GET").toUpperCase();
    return m === "POST" || m === "PUT" || m === "PATCH" || m === "DELETE";
  }

  // ---------------------------------------------------------------- fetch
  var nativeFetch = window.fetch;
  if (typeof nativeFetch === "function") {
    window.fetch = function (input, init) {
      init = init || {};

      var url = typeof input === "string" ? input : (input && input.url) || "";
      var method =
        init.method ||
        (typeof input === "object" && input && input.method) ||
        "GET";

      if (isSameOrigin(url) && isStateChanging(method)) {
        // Headers 对象、二维数组、普通对象三种写法都要能处理 ——
        // 原版用的是普通对象，但将来的代码不一定。
        try {
          var h = new Headers(init.headers || undefined);
          if (!h.has(CSRF_HEADER)) h.set(CSRF_HEADER, CSRF_VALUE);
          init.headers = h;
        } catch (e) {
          // Headers 构造失败（极端情况）时退化成普通对象写法，保证头一定带上
          var plain = init.headers || {};
          if (!plain[CSRF_HEADER] && !plain["X-Requested-With"]) {
            plain[CSRF_HEADER] = CSRF_VALUE;
          }
          init.headers = plain;
        }
      }

      return nativeFetch.call(this, input, init);
    };
  }

  // ------------------------------------------------------------------ XHR
  // 上传路径就是走它的，不能漏。
  var nativeOpen = XMLHttpRequest.prototype.open;
  var nativeSend = XMLHttpRequest.prototype.send;

  XMLHttpRequest.prototype.open = function (method, url) {
    // 记在自己身上，send 的时候才知道该不该补头
    this.__pa_method = method;
    this.__pa_url = url;
    return nativeOpen.apply(this, arguments);
  };

  XMLHttpRequest.prototype.send = function () {
    try {
      if (
        isSameOrigin(this.__pa_url) &&
        isStateChanging(this.__pa_method)
      ) {
        this.setRequestHeader(CSRF_HEADER, CSRF_VALUE);
      }
    } catch (e) {
      // setRequestHeader 在 readyState 不对时会抛。吞掉它：
      // 让请求照常发出去，由后端决定怎么处理，比在垫片里炸掉一整条链路好。
    }
    return nativeSend.apply(this, arguments);
  };

  // ------------------------------------------------------------- beacon
  // 原版没用，但它是「发一次就不管」的语义，最容易在加功能时漏掉。
  if (navigator.sendBeacon) {
    var nativeBeacon = navigator.sendBeacon.bind(navigator);
    navigator.sendBeacon = function (url, data) {
      // sendBeacon 只能发 POST，且**不能设置自定义头**。
      // 因此带 CSRF 校验的后端接口用它是发不出去的 —— 这里保持原样，
      // 让 400 暴露出来，总好过静默失效。
      return nativeBeacon(url, data);
    };
  }
})();
