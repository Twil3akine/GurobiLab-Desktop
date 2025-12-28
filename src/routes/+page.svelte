<script lang="ts">
	import { invoke } from "@tauri-apps/api/core";
	import { open } from "@tauri-apps/plugin-dialog";
	import { listen } from "@tauri-apps/api/event";
	import { marked } from "marked";
	import { onDestroy } from "svelte";

	let scriptPath = "";
	let argsStr = "100 100";
	let focusPoint = ""; // ★追加: 考察の指示

	let logs = "";
	let analysis = "";
	let status = "Execute";
	let isProcessing = false;
	let logCopyLabel = "Copy";
	let aiCopyLabel = "Copy";

	// イベント解除用の関数
	let unlisten: () => void;

	async function selectFile() {
		const file = await open({
			multiple: false,
			directory: false,
			filters: [{ name: "Python Script", extensions: ["py"] }],
		});
		if (file) scriptPath = file as string;
	}

	async function startOptimization() {
		if (!scriptPath) {
			status = "Select File First";
			return;
		}
		if (isProcessing) return;

		isProcessing = true;
		status = "Optimizing...";
		logs = ""; // ログをリセット
		analysis = "";

		// ★追加: リアルタイムログ受信
		unlisten = await listen<string>("log-output", (event) => {
			logs += event.payload + "\n";
			// 自動スクロール
			const el = document.querySelector(".log-panel pre");
			if (el) el.scrollTop = el.scrollHeight;
		});

		try {
			// Rustを実行
			const finalCleanLog = (await invoke("run_optimization", {
				scriptPath,
				argsStr,
			})) as string;

			// 完了後は整形済みログに置き換え
			logs = finalCleanLog;
			if (unlisten) unlisten();

			status = "Analyzing...";

			// ★追加: focusPoint も一緒に渡す
			const rawAnalysis = (await invoke("analyze_log", {
				log: logs,
				focusPoint, // 考察指示を渡す
			})) as string;

			analysis = rawAnalysis;
			status = "Ready";
		} catch (error) {
			status = "Error";
			logs += "\nError:\n" + String(error);
			if (unlisten) unlisten();
		} finally {
			isProcessing = false;
		}
	}

	// コンポーネント破棄時にリスナー解除
	onDestroy(() => {
		if (unlisten) unlisten();
	});

	async function copyToClipboard(text: string, target: "log" | "ai") {
		if (!text) return;
		try {
			await navigator.clipboard.writeText(text);
			if (target === "log") {
				logCopyLabel = "Copied!";
				setTimeout(() => (logCopyLabel = "Copy"), 2000);
			} else {
				aiCopyLabel = "Copied!";
				setTimeout(() => (aiCopyLabel = "Copy"), 2000);
			}
		} catch (err) {
			console.error(err);
		}
	}
</script>

<main class="container">
	<div class="controls">
		<div class="control-row">
			<div class="input-group file-group">
				<button
					class="icon-btn"
					on:click={selectFile}
					title="Select File">📂</button
				>
				<input
					bind:value={scriptPath}
					placeholder="Select Python Script (.py)..."
					readonly
					class="path-input"
				/>
			</div>
		</div>

		<div class="control-row bottom-row">
			<div class="input-group args-group">
				<span class="label">Args:</span>
				<input
					type="text"
					bind:value={argsStr}
					class="args-input"
					placeholder="e.g. 100 50"
				/>
			</div>

			<div class="input-group focus-group">
				<span class="label">🎯 Focus:</span>
				<input
					type="text"
					bind:value={focusPoint}
					class="focus-input"
					placeholder="例: Gap推移について / コスト構造の分析"
				/>
			</div>

			<button
				on:click={startOptimization}
				disabled={isProcessing || !scriptPath}
				class="run-btn"
				class:processing={isProcessing}
			>
				{status}
			</button>
		</div>
	</div>

	<div class="panels">
		<div class="panel log-panel">
			<div class="panel-header">
				<h2>Execution Logs</h2>
				<button
					class="copy-btn"
					on:click={() => copyToClipboard(logs, "log")}
					>{logCopyLabel}</button
				>
			</div>
			<pre>{logs}</pre>
		</div>

		<div class="panel ai-panel">
			<div class="panel-header">
				<h2>AI Analysis</h2>
				<button
					class="copy-btn"
					on:click={() => copyToClipboard(analysis, "ai")}
					>{aiCopyLabel}</button
				>
			</div>
			<div class="markdown-content markdown-body">
				{#await marked.parse(analysis)}
					<p>Rendering...</p>
				{:then html}
					{@html html}
				{/await}
			</div>
		</div>
	</div>
</main>

<style>
	:global(body) {
		margin: 0;
		background: #1a1b26;
		color: #a9b1d6;
		font-family: "Segoe UI", sans-serif;
	}
	.container {
		height: 100vh;
		display: flex;
		flex-direction: column;
		padding: 15px;
		box-sizing: border-box;
	}
	h1 {
		margin: 0 0 15px 0;
		font-size: 1.2rem;
		color: #7aa2f7;
		letter-spacing: 1px;
	}

	.controls {
		display: flex;
		flex-direction: column;
		gap: 10px;
		margin-bottom: 15px;
		background: #24283b;
		padding: 15px;
		border-radius: 8px;
		border: 1px solid #414868;
	}
	.control-row {
		display: flex;
		gap: 10px;
		width: 100%;
	}
	.bottom-row {
		align-items: stretch;
	}

	.input-group {
		display: flex;
		align-items: center;
		gap: 8px;
		background: #1a1b26;
		border: 1px solid #414868;
		border-radius: 4px;
		padding: 4px 8px;
	}

	.file-group {
		flex: 1;
	}
	.args-group {
		width: 120px;
		flex-shrink: 0;
	}
	.focus-group {
		flex: 1;
	} /* Focus欄を広く取る */

	input {
		background: transparent;
		border: none;
		color: #c0caf5;
		padding: 8px;
		font-family: "Consolas", monospace;
		outline: none;
		width: 100%;
	}
	.path-input {
		cursor: pointer;
	}
	.focus-input {
		font-family: "Segoe UI", sans-serif;
	}

	.label {
		font-weight: bold;
		color: #e0af68;
		font-size: 0.9rem;
		white-space: nowrap;
	}

	button {
		cursor: pointer;
		border: none;
		border-radius: 4px;
		font-weight: bold;
		transition: 0.2s;
	}
	.icon-btn {
		background: transparent;
		font-size: 1.2rem;
		padding: 0 5px;
	}
	.run-btn {
		padding: 0 20px;
		background: #7aa2f7;
		color: #1a1b26;
		white-space: nowrap;
		min-width: 100px;
	}
	.run-btn:hover {
		opacity: 0.9;
	}
	.run-btn:disabled {
		background: #2f334d;
		color: #565f89;
		cursor: not-allowed;
	}
	.run-btn.processing {
		animation: pulse 2s infinite;
	}

	.panels {
		display: flex;
		gap: 15px;
		flex: 1;
		min-height: 0;
	}
	.panel {
		flex: 1;
		background: #24283b;
		border-radius: 8px;
		padding: 15px;
		display: flex;
		flex-direction: column;
		border: 1px solid #414868;
	}
	.panel-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		border-bottom: 1px solid #414868;
		padding-bottom: 10px;
		margin-bottom: 10px;
	}
	h2 {
		margin: 0;
		font-size: 0.95rem;
		color: #bb9af7;
	}

	.copy-btn {
		background: transparent;
		border: 1px solid #414868;
		color: #565f89;
		padding: 2px 10px;
		font-size: 0.75rem;
		border-radius: 4px;
	}
	.copy-btn:hover {
		color: #c0caf5;
		border-color: #c0caf5;
	}

	pre {
		flex: 1;
		overflow-y: auto;
		font-family: "Consolas", monospace;
		font-size: 0.85rem;
		line-height: 1.4;
		color: #9ece6a;
		margin: 0;
		white-space: pre-wrap;
	}

	.markdown-content {
		flex: 1;
		overflow-y: auto;
		color: #c0caf5;
		font-size: 0.9rem;
		line-height: 1.6;
	}
	.markdown-content :global(h1),
	.markdown-content :global(h2),
	.markdown-content :global(h3) {
		color: #7aa2f7;
		margin-top: 1em;
		border-bottom: 1px solid #414868;
		padding-bottom: 0.3em;
	}
	.markdown-content :global(p) {
		margin-bottom: 1em;
	}
	.markdown-content :global(ul),
	.markdown-content :global(ol) {
		padding-left: 1.5em;
		margin-bottom: 1em;
	}
	.markdown-content :global(li) {
		margin-bottom: 0.3em;
	}
	.markdown-content :global(strong) {
		color: #e0af68;
	}
	.markdown-content :global(code) {
		background: #1a1b26;
		padding: 2px 6px;
		border-radius: 4px;
		font-family: "Consolas", monospace;
		color: #ff9e64;
	}
	.markdown-content :global(pre) {
		background: #1a1b26;
		padding: 10px;
		border-radius: 6px;
		overflow-x: auto;
		margin-bottom: 1em;
	}
	.markdown-content :global(pre code) {
		background: transparent;
		padding: 0;
		color: #c0caf5;
	}

	@keyframes pulse {
		0% {
			opacity: 1;
		}
		50% {
			opacity: 0.5;
		}
		100% {
			opacity: 1;
		}
	}

	:global(::-webkit-scrollbar) {
		display: none;
	}

	:global(*) {
		scrollbar-width: none; /* Firefox */
		-ms-overflow-style: none; /* IE/Edge */
	}
</style>
