/* Runs before module imports, so a failed bundle cannot leave an empty window. */
(() => {
  let ready = false;
  const fail = failure => {
    if (ready) return;
    const root = document.getElementById('root');
    if (!root) { document.addEventListener('DOMContentLoaded', () => fail(failure), {once:true}); return; }
    root.replaceChildren();
    const main = document.createElement('main');
    main.className = 'cadpad-startup';
    main.setAttribute('role', 'alert');
    const title = document.createElement('h1');
    title.textContent = 'Cadpad couldn’t start';
    const explanation = document.createElement('p');
    explanation.textContent = 'Reload to try again. Your projects have not been deleted.';
    const details = document.createElement('details');
    const summary = document.createElement('summary');
    summary.textContent = 'Error details';
    const text = document.createElement('pre');
    text.textContent = failure instanceof Error ? [failure.message, failure.stack].filter(Boolean).join('\n') : String(failure);
    details.append(summary, text);
    const reload = document.createElement('button');
    reload.type = 'button';reload.textContent = 'Reload Cadpad';
    reload.addEventListener('click', () => location.reload());
    main.append(title, explanation, reload, details);root.append(main);
  };
  window.cadpadStartup = {fail, ready: () => {ready = true;}};
  window.addEventListener('error', event => {
    if (event.target instanceof HTMLScriptElement) fail(new Error('Could not load ' + event.target.src));
    else if (event.error) fail(event.error);
  }, true);
  window.addEventListener('unhandledrejection', event => fail(event.reason));
})();
