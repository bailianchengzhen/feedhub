package feedhub.source.coolapk

/**
 * 酷安评论验证码的浏览器页面（网易易盾 NEC）。
 *
 * 服务器会下发
 * `captchaId`；用户本人在官方验证码里完成一次，之后拿到的 `validate` 会被拼成
 * `NEC:<captchaId 前 8 位>:<validate>`，作为 `validate` cookie 回传给酷安。
 *
 * 这里只生成 HTML，不联网、不解验证码：
 * - App 桌面端把页面挂在本地 127.0.0.1 的临时 HTTP 服务上，[submitUrl] 指回 `/proof?...`，
 *   用户完成验证后页面自动回传，App 继续重试评论请求；
 * - cli 没有本地服务，[submitUrl] 给 null，页面把 proof 显示在文本框里让用户粘贴。
 *
 * 页面里的 `initNECaptcha` 参数照 coolapk-desktop `src/utils/neteaseCaptcha.ts`（MIT）：
 * `mode=popup`、`apiVersion=2`、`protocol=https`、`width=320px`、`lang=zh-CN`，
 * 弹窗用 **API v2 的 `instance.verify()`**（不是旧版的 `popUp()`），容器在点按钮时才创建、
 * 平时不挡住页面上的按钮。
 *
 * App 里（有 [submitUrl]）**不显示介绍、页面加载完就直接弹出滑块**；滑块被关掉 / 没弹出来时才露出「重新打开验证码」。滑块还是用户本人划，代码不解、不点。
 * cli（没有 [submitUrl]）保留说明页：那边要把 proof 手动粘回去。
 */
object CoolapkCaptchaPage {
    fun html(captchaId: String, submitUrl: String?): String {
        val id8 = captchaId.take(8)
        val submit = submitUrl?.let { "'${escapeJs(it)}'" } ?: "null"
        return """
<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>酷安评论验证</title>
<style>
  :root { color-scheme: light dark; }
  body { margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center;
         font: 15px/1.6 system-ui, -apple-system, "Noto Sans CJK SC", sans-serif;
         background: #f5f5f5; color: #1b1b1b; }
  main { width: min(560px, calc(100vw - 32px)); background: #fff; border-radius: 14px; padding: 24px 24px 20px;
         box-shadow: 0 8px 30px rgb(0 0 0 / 10%); }
  h1 { font-size: 18px; margin: 0 0 8px; }
  p { margin: 8px 0; color: #555; }
  button { appearance: none; border: 0; border-radius: 999px; padding: 10px 22px; font: inherit;
           background: #0a7cff; color: #fff; cursor: pointer; }
  button:disabled { opacity: .5; cursor: default; }
  #status { margin-top: 14px; min-height: 1.6em; color: #0a7cff; }
  #proofBox { display: none; margin-top: 12px; }
  body.auto { background: transparent; }
  body.auto main { background: transparent; box-shadow: none; text-align: center; }
  body.auto .intro, body.auto #start { display: none; }
  body.auto.retry #start { display: inline-block; }
  textarea { width: 100%; box-sizing: border-box; height: 74px; resize: vertical; padding: 8px;
             border: 1px solid #ccc; border-radius: 8px; font: 12px/1.4 ui-monospace, monospace; }
  @media (prefers-color-scheme: dark) {
    body { background: #161616; color: #eee; }
    main { background: #232323; box-shadow: none; }
    p { color: #bbb; }
    textarea { background: #1a1a1a; color: #eee; border-color: #444; }
  }
</style>
</head>
<body>
<main>
  <h1 class="intro">酷安评论需要一次人机验证</h1>
  <p class="intro">酷安要求第三方客户端在读取评论前先完成验证。验证一次，之后这个会话的评论页都能看。</p>
  <p class="intro">验证由网易易盾提供，结果只回传给本机的 feedhub，不会保存到别处。</p>
  <p><button id="start" type="button">开始验证</button></p>
  <div id="status">准备就绪。</div>
  <div id="proofBox">
    <p>请把下面这一整行复制回 feedhub：</p>
    <textarea id="proof" readonly></textarea>
  </div>
</main>
<script src="https://cstaticdun.126.net/load.min.js" defer></script>
<script>
const CAPTCHA_ID = '${escapeJs(captchaId)}';
const PROOF_PREFIX = 'NEC:${escapeJs(id8)}:';
const SUBMIT_URL = $submit;

var captchaContainer = null;
// App 里直接弹滑块：不要介绍页，按钮只在滑块关掉 / 没弹出来之后露出来（文案改成"重新打开"）
var AUTO = SUBMIT_URL !== null;
if (AUTO) {
  document.body.classList.add('auto');
  document.getElementById('start').textContent = '重新打开验证码';
}
function allowRetry() {
  document.getElementById('start').disabled = false;
  if (AUTO) document.body.classList.add('retry');
}

function setStatus(text) { document.getElementById('status').textContent = text; }
function showProof(proof, note) {
  document.getElementById('proof').value = proof;
  document.getElementById('proofBox').style.display = 'block';
  setStatus(note || '验证成功。请把下面这一整行复制回 feedhub。');
}
function clearContainer() {
  if (captchaContainer && captchaContainer.parentNode) {
    captchaContainer.parentNode.removeChild(captchaContainer);
  }
  captchaContainer = null;
}
function start() {
  if (!window.initNECaptcha) {
    setStatus('验证码脚本还没加载好，请检查网络后重试。');
    allowRetry();
    return;
  }
  document.getElementById('start').disabled = true;
  document.body.classList.remove('retry');
  setStatus('正在打开验证码…');
  // API v2 的 popup 需要一个大一点的挂载点；只在验证期间创建，平时不挡按钮
  captchaContainer = document.createElement('div');
  captchaContainer.style.position = 'fixed';
  captchaContainer.style.inset = '0';
  captchaContainer.style.zIndex = '99999';
  document.body.appendChild(captchaContainer);
  try {
    window.initNECaptcha({
      captchaId: CAPTCHA_ID,
      element: captchaContainer,
      mode: 'popup',
      apiVersion: 2,
      protocol: 'https',
      width: '320px',
      lang: 'zh-CN',
      onReady: function (instance) {
        // v2 的 onReady 只表示实例可用；真正弹出在下面的 onload 里调 verify()
      },
      onVerify: function (err, data) {
        allowRetry();
        clearContainer();
        if (err || !data || !data.validate) {
          setStatus('验证没有完成：' + (err && (err.message || err) || '没有拿到 validate'));
          return;
        }
        var proof = PROOF_PREFIX + data.validate;
        if (!SUBMIT_URL) {
          showProof(proof, null);
          return;
        }
        setStatus('验证通过，正在回传给 feedhub…');
        fetch(SUBMIT_URL + '&value=' + encodeURIComponent(proof), { cache: 'no-store' })
          .then(function (response) {
            if (!response.ok) throw new Error('HTTP ' + response.status);
            return response.text();
          })
          .then(function () { setStatus('验证成功，可以关闭这个页面，回到 feedhub。'); })
          .catch(function (e) { showProof(proof, '自动回传失败（' + e + '）；请把下面这一整行复制回 feedhub。'); });
      },
      onClose: function () {
        allowRetry();
        clearContainer();
        setStatus('验证码已关闭；可以点"开始验证"重来。');
      }
    }, function (instance) {
      // 初始化成功的回调：API v2 popup 用 verify() 弹出（旧版才用 popUp()）
      try {
        if (instance && typeof instance.verify === 'function') {
          setStatus('请在验证码窗口里完成验证…');
          instance.verify();
        } else {
          allowRetry();
          clearContainer();
          setStatus('验证码实例不支持弹出验证，请刷新页面重试。');
        }
      } catch (e) {
        allowRetry();
        clearContainer();
        setStatus('唤起验证码失败：' + e);
      }
    }, function (err) {
      allowRetry();
      clearContainer();
      setStatus('验证码初始化失败：' + (err && (err.message || err)));
    });
  } catch (e) {
    allowRetry();
    clearContainer();
    setStatus('验证码初始化异常：' + e);
  }
}
document.getElementById('start').addEventListener('click', start);
// 易盾脚本是 defer 的：等它跑完（load）再弹，脚本没加载上 start() 自己会说并露出重试
if (AUTO) window.addEventListener('load', start);
</script>
</body>
</html>
        """.trimIndent()
    }

    /** 放进 JS 单引号字符串里：反斜杠、单引号、换行都要转义。 */
    private fun escapeJs(value: String): String =
        value.replace("\\", "\\\\").replace("'", "\\'").replace("\n", "\\n").replace("\r", "")
}
