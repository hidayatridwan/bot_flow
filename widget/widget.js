// Minimal embeddable RAG chat widget.
// Usage: ChatWidget.init({ apiBase: 'http://localhost:3000', publicKey: 'pk_...', title: 'BotFlow' });
(function () {
    const ICONS = {
        mark: '<svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor"><path d="M3 3h5.2l6 8.9L8.6 21H3.4l5.3-9L3 3zm10.4 0h5.2l-3.3 5.6-2.6-3.9L13.4 3zm2.6 12.4 2.6 3.9-1.3 1.7h5.2l-4.6-7-1.9 1.4z"/></svg>',
        compose: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 20h4l10-10a2.1 2.1 0 0 0-3-3L5 17v3z"/><path d="M14 6l3 3"/></svg>',
        chevron: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M6 9l6 6 6-6"/></svg>',
        send: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 19V5"/><path d="M5 12l7-7 7 7"/></svg>',
    };

    const ChatWidget = {
        init(config) {
            this.apiBase = config.apiBase.replace(/\/$/, '');
            this.publicKey = config.publicKey;
            this.title = config.title || 'BotFlow';
            this.conversationId = null; // set from the server's first `conversation` event
            this._injectStyles();
            this._buildUi();
        },

        _injectStyles() {
            const css = `
        #cw-button{position:fixed;bottom:20px;right:20px;width:56px;height:56px;border-radius:50%;
          background:#7c5cfc;color:#fff;border:none;cursor:pointer;box-shadow:0 4px 14px rgba(124,92,252,.4);
          z-index:9999;display:flex;align-items:center;justify-content:center}
        #cw-button svg{width:24px;height:24px}
        #cw-panel{--cw-accent:#7c5cfc;--cw-user-bg:#ddd6fe;--cw-bot-bg:#f3f4f6;
          --cw-text:#111827;--cw-muted:#6b7280;--cw-border:#e8e8ed;
          position:fixed;bottom:88px;right:20px;width:360px;height:560px;max-height:70vh;
          display:none;flex-direction:column;background:#fff;border:1px solid var(--cw-border);
          border-radius:16px;box-shadow:0 12px 32px rgba(17,24,39,.14);overflow:hidden;z-index:9999;
          font-family:system-ui,-apple-system,"Segoe UI",sans-serif;color:var(--cw-text)}
        #cw-panel.open{display:flex}
        #cw-header{display:flex;align-items:center;gap:10px;padding:14px 16px;
          background:#fff;border-bottom:1px solid var(--cw-border)}
        #cw-title{font-weight:700;font-size:17px}
        .cw-hdr-btn{background:none;border:none;padding:4px;cursor:pointer;color:var(--cw-muted);
          display:flex;align-items:center;border-radius:6px}
        .cw-hdr-btn:hover{color:var(--cw-text);background:var(--cw-bot-bg)}
        .cw-hdr-btn.cw-first{margin-left:auto}
        #cw-log{flex:1;overflow-y:auto;padding:8px 16px}
        .cw-row{display:flex;gap:10px;margin:14px 0;align-items:flex-start}
        .cw-row-user{justify-content:flex-end}
        .cw-avatar{flex:0 0 32px;width:32px;height:32px;border-radius:50%;background:var(--cw-accent);
          color:#fff;display:flex;align-items:center;justify-content:center}
        .cw-avatar svg{width:16px;height:16px}
        .cw-col{display:flex;flex-direction:column;min-width:0;max-width:82%}
        .cw-bubble{padding:12px 14px;border-radius:18px;font-size:14.5px;line-height:1.5}
        .cw-row-user .cw-bubble{max-width:78%}
        .cw-user{background:var(--cw-user-bg);color:var(--cw-text)}
        .cw-bot{background:var(--cw-bot-bg);white-space:pre-wrap;overflow-wrap:anywhere}
        #cw-form{display:flex;align-items:center;gap:8px;padding:10px 12px;border-top:1px solid var(--cw-border)}
        #cw-input{flex:1;min-width:0;border:none;background:transparent;padding:8px;
          font:inherit;font-size:14.5px;color:var(--cw-text);outline:none}
        #cw-input::placeholder{color:var(--cw-muted)}
        #cw-send{flex:0 0 38px;width:38px;height:38px;border:none;border-radius:10px;
          background:var(--cw-bot-bg);color:var(--cw-muted);cursor:not-allowed;
          display:flex;align-items:center;justify-content:center}
        #cw-send:not(:disabled){background:var(--cw-accent);color:#fff;cursor:pointer}
        .cw-sources{margin-top:8px;display:flex;flex-direction:column;gap:4px}
        .cw-sources-hdr{font-size:11px;font-weight:600;color:var(--cw-muted);
          text-transform:uppercase;letter-spacing:.04em}
        .cw-source{font-size:13px;border:1px solid var(--cw-border);border-radius:10px;
          overflow:hidden;background:#fff}
        .cw-source-sum{display:flex;align-items:center;gap:8px;padding:6px 10px;cursor:pointer;
          list-style:none}
        .cw-source-sum::-webkit-details-marker{display:none}
        .cw-chip{font-weight:700;color:var(--cw-accent);font-size:12px}
        .cw-score{color:var(--cw-muted);font-size:12px;font-variant-numeric:tabular-nums}
        .cw-source-text{padding:0 10px 8px;color:var(--cw-text);white-space:pre-wrap;
          overflow-wrap:anywhere;line-height:1.45}
        .cw-bot-muted{color:var(--cw-muted);font-style:italic}`;
            const s = document.createElement('style'); s.textContent = css; document.head.appendChild(s);
        },

        _buildUi() {
            const btn = document.createElement('button'); btn.id = 'cw-button'; btn.innerHTML = ICONS.mark;
            const panel = document.createElement('div'); panel.id = 'cw-panel';
            panel.innerHTML = `
        <div id="cw-header">
          <div class="cw-avatar">${ICONS.mark}</div>
          <div id="cw-title"></div>
          <button id="cw-new" class="cw-hdr-btn cw-first" type="button" title="New chat">${ICONS.compose}</button>
          <button id="cw-close" class="cw-hdr-btn" type="button" title="Minimise">${ICONS.chevron}</button>
        </div>
        <div id="cw-log"></div>
        <form id="cw-form">
          <input id="cw-input" placeholder="Type your question…" autocomplete="off"/>
          <button id="cw-send" type="submit" disabled>${ICONS.send}</button>
        </form>`;
            panel.querySelector('#cw-title').textContent = this.title;
            document.body.appendChild(btn); document.body.appendChild(panel);
            btn.onclick = () => panel.classList.toggle('open');
            this.log = panel.querySelector('#cw-log');
            this.input = panel.querySelector('#cw-input');
            this.send = panel.querySelector('#cw-send');
            panel.querySelector('#cw-close').onclick = () => panel.classList.remove('open');
            // Clearing the transcript must drop the conversation too, or the server keeps
            // resolving follow-ups against history the user can no longer see.
            panel.querySelector('#cw-new').onclick = () => { this.log.innerHTML = ''; this.conversationId = null; };
            this.input.oninput = () => { this.send.disabled = !this.input.value.trim(); };
            panel.querySelector('#cw-form').onsubmit = (e) => { e.preventDefault(); this._send(); };
        },

        // Builds a message row and returns the bubble the caller writes text into.
        _append(cls, text) {
            const row = document.createElement('div');
            const bubble = document.createElement('div');
            bubble.className = 'cw-bubble ' + cls;
            bubble.textContent = text;
            if (cls === 'cw-user') {
                row.className = 'cw-row cw-row-user';
                row.appendChild(bubble);
            } else {
                row.className = 'cw-row cw-row-bot';
                const avatar = document.createElement('div');
                avatar.className = 'cw-avatar'; avatar.innerHTML = ICONS.mark;
                const col = document.createElement('div'); col.className = 'cw-col';
                col.appendChild(bubble);
                row.append(avatar, col);
            }
            this.log.appendChild(row); this.log.scrollTop = this.log.scrollHeight; return bubble;
        },

        async _send() {
            const query = this.input.value.trim(); if (!query) return;
            this.input.value = '';
            this.send.disabled = true;
            this._append('cw-user', query);
            const bot = this._append('cw-bot', '');
            // One turn's accumulating state. `tokens` is counted, not inferred from `answer`: a
            // whitespace-only reply is still the model speaking, and judging the prose is not our
            // job — only noticing when there is none of it at all.
            const turn = { bot, answer: '', tokens: 0, sources: [], done: false, error: false, sourcesEl: null };
            try {
                const body = { query };
                if (this.conversationId) body.conversation_id = this.conversationId;
                const res = await fetch(`${this.apiBase}/ask/stream`, {
                    method: 'POST',
                    headers: { authorization: `Bearer ${this.publicKey}`, 'content-type': 'application/json' },
                    body: JSON.stringify(body),
                });
                if (!res.ok) {
                    // A stranger must not read `Error 403`. The tenant needs the status (a 403 is an
                    // allowed_origins miss they must fix); the visitor needs a sentence. Log the one,
                    // show the other. 429 is the single status a visitor can act on, so it gets its own.
                    console.error('[widget] ask failed:', res.status);
                    turn.error = true;
                    bot.textContent = res.status === 429
                        ? 'The assistant is busy right now — please wait a moment and try again.'
                        : "The assistant isn't available right now. Please try again later.";
                    bot.classList.add('cw-bot-muted');
                    return;
                }

                const reader = res.body.getReader();
                const decoder = new TextDecoder();
                let buffer = '';
                while (true) {
                    const { value, done } = await reader.read();
                    if (done) break;
                    buffer += decoder.decode(value, { stream: true });
                    let i;
                    while ((i = buffer.indexOf('\n\n')) !== -1) {
                        const frame = this._parseEvent(buffer.slice(0, i));
                        buffer = buffer.slice(i + 2);
                        this._onEvent(turn, frame.event, frame.data);
                    }
                }
            } catch (err) {
                console.error('[widget] stream error:', err);
                turn.error = true;
                bot.textContent = 'The connection was interrupted. Please ask again.';
                bot.classList.add('cw-bot-muted');
            }
            this._finalize(turn);
        },

        // Dispatch one SSE frame into the turn's state.
        _onEvent(turn, event, data) {
            switch (event) {
                case 'conversation':
                    this.conversationId = data;
                    break;
                case 'sources':
                    // Citations arrive *before* the first token — the API knows them the moment
                    // retrieval returns. Rendering them as they land is a real property of the
                    // product, and the first time this widget has ever shown them (invariant 5).
                    try { turn.sources = JSON.parse(data); } catch { turn.sources = []; }
                    this._renderSources(turn);
                    break;
                case 'token':
                    turn.tokens += 1;
                    turn.answer += data;
                    turn.bot.textContent = turn.answer;
                    this.log.scrollTop = this.log.scrollHeight;
                    break;
                case 'error':
                    // Invariant 16: the frame's text is not ours to render, even though the API now
                    // sends a fixed string. Show our own copy; keep the detail in the console.
                    console.error('[widget] the api reported a stream failure:', data);
                    turn.error = true;
                    turn.bot.textContent = 'Something went wrong. Please ask again.';
                    turn.bot.classList.add('cw-bot-muted');
                    break;
                case 'done':
                    // The only terminal frame. Before this, the loop ended only when the socket
                    // closed, so a finished answer and a dropped connection were indistinguishable.
                    turn.done = true;
                    break;
            }
        },

        // Render (or re-render) the citations under the answer. Ported from web/'s sources.ts.
        // The one rule that must not drift: the chip shows `index` FROM THE FIELD, never the array
        // position. The model may not write `[n]` markers (invariant 5), so this number is the only
        // thing tying the answer back to a passage — and the API sends 1..n in order today, so a
        // `#{i+1}` would look right in every case that did not check this exact field. Unlike the
        // dashboard, the widget holds only a `pk_` and cannot call GET /documents, so it names no
        // filename — the chip, the score and the passage are the honest subset it can show.
        _renderSources(turn) {
            if (!turn.sources.length) return;
            let box = turn.sourcesEl;
            if (!box) {
                box = document.createElement('div');
                box.className = 'cw-sources';
                turn.bot.parentElement.appendChild(box); // the .cw-col beside the avatar
                turn.sourcesEl = box;
            }
            box.innerHTML = '';
            const hdr = document.createElement('div');
            hdr.className = 'cw-sources-hdr'; hdr.textContent = 'Sources';
            box.appendChild(hdr);
            for (const s of turn.sources) {
                const item = document.createElement('details'); item.className = 'cw-source';
                const sum = document.createElement('summary'); sum.className = 'cw-source-sum';
                const chip = document.createElement('span');
                chip.className = 'cw-chip'; chip.textContent = `[${s.index}]`;
                const score = document.createElement('span');
                // Two decimals, not a percentage: a cosine score is not a probability, and `54%`
                // invites "54% confident", a claim the number does not make. Matches sources.ts.
                score.className = 'cw-score'; score.textContent = Number(s.score).toFixed(2);
                sum.append(chip, score);
                const text = document.createElement('div');
                text.className = 'cw-source-text';
                text.textContent = s.text; // textContent, never innerHTML — the passage is document text
                item.append(sum, text);
                box.appendChild(item);
            }
        },

        // Decide what an ended stream means, once. Mirrors ask.ts's terminal logic.
        _finalize(turn) {
            if (turn.error) return; // a message is already shown
            if (!turn.done) {
                // Ended without the `done` sentinel — a dropped socket or a killed connection.
                // Whatever tokens arrived are true and stay on screen; only a wholly empty answer
                // needs a word, or the bubble reads as a broken page.
                if (turn.tokens === 0) {
                    turn.bot.textContent = 'The answer was cut short. Please ask again.';
                    turn.bot.classList.add('cw-bot-muted');
                }
                return;
            }
            // Completed cleanly, retrieval found passages, and the model still said nothing. Not a
            // refusal — that carries its canned sentence and has no sources. The cause is upstream: a
            // reasoning model can spend its whole token budget thinking and emit no content, and
            // nothing errors, so the API rightly yields `done`. Only the client can see the silence.
            if (turn.sources.length > 0 && turn.tokens === 0) {
                turn.bot.textContent = "I found relevant information but couldn't produce an answer. Please ask again.";
                turn.bot.classList.add('cw-bot-muted');
            }
        },

        _parseEvent(raw) {
            let event = 'message'; const data = [];
            for (const line of raw.split('\n')) {
                if (line.startsWith('event:')) event = line.slice(6).trim();
                else if (line.startsWith('data:')) data.push(line.slice(5).replace(/^ /, ''));
            }
            return { event, data: data.join('\n') };
        },

    };
    window.ChatWidget = ChatWidget;
})();
