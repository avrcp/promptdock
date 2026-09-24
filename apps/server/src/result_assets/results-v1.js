'use strict';
const article = document.getElementById('result');
const rawPanel = document.getElementById('raw-panel');
const rawText = document.getElementById('raw-text');
const status = document.getElementById('action-status');
const rawLink = document.getElementById('raw-link');
const copy = document.getElementById('copy-all');
const toggle = document.getElementById('toggle-raw');
let unloaded = false;
async function loadRaw() {
  const response = await fetch(rawLink.href, {cache:'no-store', credentials:'omit', redirect:'error'});
  if (!response.ok || !response.headers.get('content-type')?.startsWith('text/plain')) throw new Error('unavailable');
  const value = await response.text();
  if (unloaded) throw new Error('unloaded');
  return value;
}
function showRaw(value) {
  rawText.value = value;
  rawPanel.hidden = false;
  article.hidden = true;
  toggle.textContent = '显示排版';
  toggle.setAttribute('aria-pressed','true');
}
copy.hidden = toggle.hidden = false;
copy.addEventListener('click', async () => {
  copy.disabled = true;
  status.textContent = '';
  try {
    const value = await loadRaw();
    try { await navigator.clipboard.writeText(value); status.textContent = '全文已复制。'; }
    catch { showRaw(value); rawText.focus(); rawText.select(); status.textContent = '未能访问剪贴板，已选择原文，请手动复制。'; }
  } catch { status.textContent = '原文暂不可读取，链接可能已失效。请重新打开确认。'; }
  finally { copy.disabled = false; }
});
toggle.addEventListener('click', async () => {
  if (!rawPanel.hidden) { rawPanel.hidden=true; article.hidden=false; toggle.textContent='显示原文'; toggle.setAttribute('aria-pressed','false'); return; }
  toggle.disabled=true;
  try { showRaw(await loadRaw()); status.textContent=''; }
  catch { status.textContent='原文暂不可读取，请重新打开确认。'; }
  finally { toggle.disabled=false; }
});
for (const code of article.querySelectorAll('pre > code')) {
  const value = code.textContent;
  const button = document.createElement('button');
  button.type='button'; button.className='code-copy'; button.textContent='复制代码';
  button.addEventListener('click', async () => {
    try { await navigator.clipboard.writeText(value); status.textContent='代码已复制。'; }
    catch { const range=document.createRange(); range.selectNodeContents(code); const selection=getSelection(); selection.removeAllRanges(); selection.addRange(range); status.textContent='未能访问剪贴板，已选择代码，请手动复制。'; }
  });
  code.parentElement.before(button);
}
for (const time of document.querySelectorAll('time[data-ms]')) {
  const date = new Date(Number(time.dataset.ms));
  if (!Number.isNaN(date.getTime())) { time.dateTime=date.toISOString(); time.textContent=date.toLocaleString(); }
}
addEventListener('pagehide', () => { unloaded=true; rawText.value=''; article.replaceChildren(); });
addEventListener('pageshow', event => { if (event.persisted) location.reload(); });
