<script lang="ts">
	import { invoke } from "@tauri-apps/api/core";
	import { open } from "@tauri-apps/plugin-dialog";
	import { listen } from "@tauri-apps/api/event";
	import { marked } from "marked";
	import { onDestroy, onMount, tick } from "svelte";
	import Chart from "chart.js/auto";

	// --- 変数定義 ---
	let scriptPath = "";
	let argsStr = "";
	let focusPoint = "";
	let apiKey = "";
	let isMenuOpen = false;

	let logs = "";
	let analysis = "";
	let status = "Ready";
	let isProcessing = false;
	let currentPid: number | null = null;

	let unlistenLog: () => void;
	let unlistenPid: () => void;

	let activeTab: "main" | "history" | "settings" = "main";
	let historyList: any[] = [];

	// グラフ関連
	let chartCanvas: HTMLCanvasElement;
	let chartInstance: Chart | null = null;

	// プレビューモードかどうかのフラグ
	let isPreview = false;
	let tokenStats = "";

	// プレビュー前の解析結果を避難させておく変数
	let lastAnalysis = "";

	// --- ライフサイクル ---
	onMount(() => {
		apiKey = localStorage.getItem("gurobi_app_apikey") || "";
		const savedHist = localStorage.getItem("gurobi_app_history");
		if (savedHist) historyList = JSON.parse(savedHist);
	});

	onDestroy(() => {
		cleanupListeners();
		if (chartInstance) chartInstance.destroy();
	});

	// タブがmainに切り替わったときにグラフを再初期化する
	$: if (activeTab === "main") {
		initChart();
	}

	function cleanupListeners() {
		if (unlistenLog) unlistenLog();
		if (unlistenPid) unlistenPid();
	}

	// --- グラフ初期化 ---
	async function initChart() {
		await tick();
		if (!chartCanvas) return;

		if (chartInstance) chartInstance.destroy();

		chartInstance = new Chart(chartCanvas, {
			type: "line",
			data: {
				labels: [],
				datasets: [
					{
						label: "Gap (%)",
						data: [],
						borderColor: "#7aa2f7",
						backgroundColor: "rgba(122, 162, 247, 0.1)",
						borderWidth: 2,
						tension: 0.2,
						pointRadius: 0,
						pointHoverRadius: 6,
						fill: true,
					},
				],
			},
			options: {
				responsive: true,
				maintainAspectRatio: false,
				animation: false,
				interaction: { mode: "index", intersect: false },
				scales: {
					x: { display: false },
					y: {
						type: "logarithmic",
						// 下限設定
						min: 0.0001,
						grid: { color: "#2f334d" },
						ticks: {
							color: "#565f89",
							maxTicksLimit: 20,
							callback: function (value, index, values) {
								const num = Number(value);
								const log10 = Math.log10(num);
								// 10の乗数のみ表示
								if (
									Math.abs(log10 - Math.round(log10)) < 1e-9
								) {
									return (
										num.toLocaleString("en-US", {
											maximumSignificantDigits: 1,
										}) + "%"
									);
								}
								return null;
							},
						},
					},
				},
				plugins: { legend: { display: false } },
			},
		});

		if (logs) rebuildGraphFromLogs(logs);
	}

	function rebuildGraphFromLogs(fullLog: string) {
		if (!chartInstance) return;
		chartInstance.data.labels = [];
		chartInstance.data.datasets[0].data = [];
		fullLog.split("\n").forEach((line) => parseLogForGraph(line, false));
		chartInstance.update();
	}

	// --- 計算実行 ---
	async function startOptimization() {
		if (!scriptPath) {
			status = "No File Selected";
			return;
		}
		if (isProcessing) return;

		isProcessing = true;
		status = "Running...";
		logs = "";
		analysis = "";

		if (chartInstance) {
			chartInstance.data.labels = [];
			chartInstance.data.datasets[0].data = [];
			chartInstance.update();
		}

		unlistenLog = await listen<string>("log-output", (event) => {
			const line = event.payload;
			logs += line + "\n";
			parseLogForGraph(line, true);
			const el = document.querySelector(".log-panel pre");
			if (el) el.scrollTop = el.scrollHeight;
		});

		unlistenPid = await listen<number>("process-pid", (event) => {
			currentPid = event.payload;
		});

		try {
			const finalLog = (await invoke("run_optimization", {
				scriptPath,
				argsStr,
			})) as string;

			logs = finalLog;
			cleanupListeners();

			await askAI();
			saveHistory();
		} catch (error) {
			status = "Error";
			logs += "\nError:\n" + String(error);
		} finally {
			isProcessing = false;
			currentPid = null;
			cleanupListeners();
		}
	}

	// --- デバッグ＆AI解析 ---
	async function showPromptPreview() {
		if (!logs) return;
		analysis = "Generating prompt preview...";
		isPreview = true;
		try {
			const rawPrompt = (await invoke("debug_prompt", {
				log: logs,
				focusPoint,
			})) as string;
			const charCount = rawPrompt.length;
			tokenStats = `Length: ${charCount} chars`;
			analysis = `--- PROMPT PREVIEW (${tokenStats}) ---\n\n${rawPrompt}`;
		} catch (e) {
			analysis = "Error generating preview: " + e;
		}
	}

	// ★変更: プレビューの表示/非表示を切り替える関数
	async function togglePreview() {
		if (isPreview) {
			// ■ 戻る処理
			analysis = lastAnalysis; // 避難させていた内容を戻す
			isPreview = false;
		} else {
			// ■ プレビュー表示処理
			if (!logs) return;

			lastAnalysis = analysis; // 現在の表示（解析結果）を避難

			analysis = "Generating prompt preview...";
			isPreview = true;

			try {
				const rawPrompt = (await invoke("debug_prompt", {
					log: logs,
					focusPoint,
				})) as string;

				const charCount = rawPrompt.length;
				tokenStats = `Length: ${charCount} chars`;
				analysis = `--- PROMPT PREVIEW (${tokenStats}) ---\n\n${rawPrompt}`;
			} catch (e) {
				analysis = "Error generating preview: " + e;
			}
		}
	}

	// AI解析実行時はプレビューモードを強制解除
	async function askAI() {
		if (!logs) {
			status = "No Logs";
			return;
		}

		// もしプレビュー中なら、元の状態に戻してから解析を始める必要はないが、
		// 内部フラグはリセットしておく
		isPreview = false;

		status = "Analyzing...";
		isProcessing = true;
		try {
			const rawAnalysis = (await invoke("analyze_log", {
				log: logs,
				focusPoint,
				apiKey,
			})) as string;
			analysis = rawAnalysis;
			status = "Ready";
		} catch (error) {
			analysis += "\nAI Error: " + String(error);
			status = "Error";
		} finally {
			isProcessing = false;
		}
	}

	async function stopOptimization() {
		if (currentPid) {
			try {
				await invoke("kill_process", { pid: currentPid });
				logs += "\n[User Cancelled]\n";
				status = "Cancelled";
			} catch (e) {
				console.error(e);
			}
		}
	}

	function parseLogForGraph(line: string, doUpdate: boolean) {
		const match = line.match(/(\d+(?:\.\d+)?)%/);
		if (match && chartInstance) {
			let val = parseFloat(match[1]);
			if (!isNaN(val) && val <= 1000) {
				val = Math.max(val, 0.0001);
				const label = chartInstance.data.labels?.length || 0;
				chartInstance.data.labels?.push(label);
				chartInstance.data.datasets[0].data.push(val);
				if (doUpdate) chartInstance.update();
			}
		}
	}

	// --- その他 ---
	function saveHistory() {
		const item = {
			date: new Date().toLocaleString(),
			script: scriptPath.split(/[\\/]/).pop(),
			args: argsStr,
			log: logs,
			analysis: analysis,
		};
		historyList = [item, ...historyList].slice(0, 20);
		localStorage.setItem("gurobi_app_history", JSON.stringify(historyList));
	}

	function loadHistoryItem(item: any) {
		logs = item.log;
		analysis = item.analysis;
		activeTab = "main";
	}

	function saveSettings() {
		localStorage.setItem("gurobi_app_apikey", apiKey);
		alert("Settings Saved!");
	}

	async function selectFile() {
		const file = await open({
			multiple: false,
			directory: false,
			filters: [{ name: "Python Script", extensions: ["py"] }],
		});
		if (file) scriptPath = file as string;
	}

	async function copyToClipboard(text: string) {
		if (!text) return;
		await navigator.clipboard.writeText(text);
	}
</script>

<div class="layout">
	<aside class="sidebar" class:open={isMenuOpen}>
		<div class="sidebar-header">
			<button
				class="hamburger"
				on:click={() => (isMenuOpen = !isMenuOpen)}>☰</button
			>
			{#if isMenuOpen}<span class="brand">Gurobi MCP</span>{/if}
		</div>
		<nav>
			<button
				class:active={activeTab === "main"}
				on:click={() => (activeTab = "main")}
				title="Run"
			>
				<span class="icon">📊</span>{#if isMenuOpen}<span class="text"
						>Run</span
					>{/if}
			</button>
			<button
				class:active={activeTab === "history"}
				on:click={() => (activeTab = "history")}
				title="History"
			>
				<span class="icon">🕒</span>{#if isMenuOpen}<span class="text"
						>History</span
					>{/if}
			</button>
			<button
				class:active={activeTab === "settings"}
				on:click={() => (activeTab = "settings")}
				title="Settings"
			>
				<span class="icon">⚙️</span>{#if isMenuOpen}<span class="text"
						>Settings</span
					>{/if}
			</button>
		</nav>
	</aside>

	<main class="content">
		{#if activeTab === "main"}
			<div class="controls-area">
				<div class="control-row">
					<button class="icon-btn" on:click={selectFile}>📂</button>
					<input
						bind:value={scriptPath}
						placeholder="Script Path..."
						class="path-input"
					/>
				</div>
				<div class="control-row bottom">
					<div class="input-wrap">
						<span class="label">Args</span>
						<input
							bind:value={argsStr}
							placeholder="e.g. 100 100"
						/>
					</div>
					<div class="input-wrap focus-wrap">
						<span class="label">Focus</span>
						<input
							bind:value={focusPoint}
							placeholder="Ask AI about results..."
							on:keydown={(e) => e.key === "Enter" && askAI()}
						/>
					</div>

					<div class="action-buttons">
						{#if isProcessing && currentPid}
							<button class="stop-btn" on:click={stopOptimization}
								>⏹ Stop</button
							>
						{:else}
							<button
								class="run-btn"
								on:click={startOptimization}
								disabled={!scriptPath || isProcessing}
							>
								▶ Run
							</button>
							<button
								class="ask-btn"
								on:click={askAI}
								disabled={!logs || isProcessing}
							>
								💬 Ask AI
							</button>

							<button
								class="debug-btn"
								class:active-mode={isPreview}
								on:click={togglePreview}
								disabled={!logs || isProcessing}
								title={isPreview
									? "Close Preview"
									: "See raw prompt"}
							>
								{isPreview ? "↩" : "🔍"}
							</button>
						{/if}
					</div>
				</div>
			</div>

			<div class="chart-wrapper">
				<canvas bind:this={chartCanvas}></canvas>
			</div>

			<div class="panels">
				<div class="panel">
					<div class="panel-head">
						<span>Logs</span>
						<button
							class="copy-btn"
							on:click={() => copyToClipboard(logs)}>Copy</button
						>
					</div>
					<pre>{logs}</pre>
				</div>
				<div class="panel">
					<div class="panel-head">
						<span>{isPreview ? "Prompt Preview" : "Analysis"}</span>
						<button
							class="copy-btn"
							on:click={() => copyToClipboard(analysis)}
							>Copy</button
						>
					</div>
					<div class="markdown-body">
						{#if isPreview}
							<pre
								style="white-space: pre-wrap; word-break: break-all; color: #7dcfff; font-size: 0.8rem;">{analysis}</pre>
						{:else}
							{#await marked.parse(analysis)}
								<p class="loading">Thinking...</p>
							{:then html}
								{@html html}
							{/await}
						{/if}
					</div>
				</div>
			</div>
		{/if}

		{#if activeTab === "history"}
			<h2>Execution History</h2>
			<div class="history-list">
				{#each historyList as item}
					<button
						class="history-item"
						on:click={() => loadHistoryItem(item)}
					>
						<div class="hist-left-bar"></div>
						<div class="hist-content">
							<div class="hist-date">{item.date}</div>
							<div class="hist-detail">
								{item.script}
								<span class="hist-args">({item.args})</span>
							</div>
						</div>
						<div class="hist-arrow">👉</div>
					</button>
				{/each}
				{#if historyList.length === 0}<p>No history yet.</p>{/if}
			</div>
		{/if}

		{#if activeTab === "settings"}
			<h2>Settings</h2>
			<div class="settings-form">
				<label>Google Gemini API Key</label>
				<input
					type="password"
					bind:value={apiKey}
					placeholder="AIza..."
				/>
				<button class="save-btn" on:click={saveSettings}
					>Save Settings</button
				>
			</div>
		{/if}
	</main>
</div>

<style>
	:global(body) {
		margin: 0;
		background: #13141f;
		color: #c0caf5;
		font-family: "Segoe UI", sans-serif;
		overflow: hidden;
	}
	:global(::-webkit-scrollbar) {
		display: none;
	}

	.layout {
		display: flex;
		height: 100vh;
	}

	/* Sidebar */
	.sidebar {
		width: 60px;
		background: #1a1b26;
		border-right: 1px solid #2f334d;
		display: flex;
		flex-direction: column;
		transition: width 0.3s cubic-bezier(0.25, 0.8, 0.25, 1);
		overflow: hidden;
		flex-shrink: 0;
		z-index: 100;
	}
	.sidebar.open {
		width: 200px;
	}
	.sidebar-header {
		height: 60px;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.sidebar.open .sidebar-header {
		justify-content: flex-start;
		padding-left: 20px;
	}
	.hamburger {
		background: transparent;
		border: none;
		color: #7aa2f7;
		font-size: 1.5rem;
		cursor: pointer;
	}
	.brand {
		font-weight: bold;
		color: #c0caf5;
		margin-left: 10px;
		white-space: nowrap;
		animation: fadeIn 0.3s;
	}
	.sidebar nav button {
		width: 100%;
		height: 50px;
		background: transparent;
		border: none;
		color: #565f89;
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
		transition: 0.2s;
	}
	.sidebar.open nav button {
		justify-content: flex-start;
		padding-left: 20px;
	}
	.sidebar nav button:hover {
		background: #24283b;
		color: #c0caf5;
	}
	.sidebar nav button.active {
		color: #7aa2f7;
		background: #1f2335;
		border-right: 3px solid #7aa2f7;
	}
	.icon {
		font-size: 1.2rem;
		min-width: 60px;
		text-align: center;
	}
	.text {
		white-space: nowrap;
		animation: fadeIn 0.3s;
	}

	/* Content */
	.content {
		flex: 1;
		display: flex;
		flex-direction: column;
		padding: 20px;
		overflow: hidden;
		gap: 15px;
	}

	/* Controls */
	.controls-area {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	.control-row {
		display: flex;
		gap: 10px;
	}
	.bottom {
		align-items: stretch;
	}

	.input-wrap {
		background: #1a1b26;
		border: 1px solid #2f334d;
		display: flex;
		align-items: center;
		padding: 0 10px;
		border-radius: 6px;
	}
	.input-wrap .label {
		font-size: 0.8rem;
		font-weight: bold;
		color: #7dcfff;
		margin-right: 10px;
	}
	.focus-wrap {
		flex: 1;
		border: 1px solid #3b4261;
		transition: 0.2s;
	}
	.focus-wrap:focus-within {
		border-color: #7aa2f7;
	}

	input {
		background: transparent;
		border: none;
		color: white;
		padding: 10px;
		width: 100%;
		outline: none;
		font-family: Consolas, monospace;
	}
	.path-input {
		background: #1a1b26;
		border: 1px solid #2f334d;
		border-radius: 6px;
		flex: 1;
	}

	/* Buttons */
	.action-buttons {
		display: flex;
		gap: 10px;
	}
	button {
		cursor: pointer;
		border: none;
		border-radius: 6px;
		font-weight: bold;
		transition: 0.2s;
		white-space: nowrap;
	}
	.icon-btn {
		padding: 0 15px;
		background: #24283b;
		color: #fff;
	}

	.run-btn,
	.ask-btn,
	.stop-btn {
		padding: 0 24px 0 16px;
		min-width: 110px;
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 6px;
	}

	.run-btn {
		background: #7aa2f7;
		color: #1a1b26;
	}
	.run-btn:hover {
		background: #89b4fa;
	}

	.ask-btn {
		background: #bb9af7;
		color: #1a1b26;
	}
	.ask-btn:hover {
		background: #d0aeff;
	}

	.debug-btn {
		padding: 0 15px;
		min-width: 50px;
		background: #2f334d;
		color: #7dcfff;
		display: flex;
		align-items: center;
		justify-content: center;
		/*font-size: 1.1rem;*/
	}
	.debug-btn:hover {
		background: #3b4261;
	}

	/* プレビュー中のボタン色（黄色っぽくして注意を引くなど） */
	.debug-btn.active-mode {
		background: #e0af68;
		color: #1a1b26;
	}
	.debug-btn.active-mode:hover {
		background: #ffc777;
	}

	.stop-btn {
		background: #f7768e;
		color: #1a1b26;
		animation: pulse 1.5s infinite;
	}

	button:disabled {
		background: #2f334d;
		color: #565f89;
		cursor: not-allowed;
	}

	/* Graph */
	.chart-wrapper {
		height: 200px;
		min-height: 200px;
		background: #1a1b26;
		border: 1px solid #2f334d;
		border-radius: 8px;
		padding: 10px;
		position: relative;
	}

	/* Panels */
	.panels {
		flex: 1;
		display: flex;
		gap: 15px;
		min-height: 0;
	}
	.panel {
		flex: 1;
		background: #1a1b26;
		border: 1px solid #2f334d;
		border-radius: 8px;
		display: flex;
		flex-direction: column;
		padding: 10px;
		min-width: 0;
	}
	.panel-head {
		display: flex;
		justify-content: space-between;
		margin-bottom: 5px;
		color: #bb9af7;
		font-weight: bold;
		font-size: 0.9rem;
	}
	.copy-btn {
		background: transparent;
		color: #565f89;
		font-size: 0.8rem;
		padding: 2px 8px;
		border: 1px solid #2f334d;
	}
	.copy-btn:hover {
		color: #c0caf5;
		border-color: #c0caf5;
	}

	pre,
	.markdown-body {
		flex: 1;
		overflow-y: auto;
		font-size: 0.9rem;
		margin: 0;
		color: #c0caf5;
		line-height: 1.5;
	}
	pre {
		font-family: Consolas, monospace;
		color: #9ece6a;
		white-space: pre-wrap;
	}

	/* Markdown */
	.markdown-body :global(h1),
	.markdown-body :global(h2) {
		font-size: 1.1rem;
		color: #7aa2f7;
		border-bottom: 1px solid #2f334d;
		margin-top: 1em;
	}
	.markdown-body :global(strong) {
		color: #e0af68;
	}
	.loading {
		color: #565f89;
		font-style: italic;
	}

	/* History Styling */
	.history-list {
		display: flex;
		flex-direction: column;
		gap: 8px;
		overflow-y: auto;
		height: 100%;
	}
	.history-item {
		background: #1a1b26;
		padding: 0;
		border: 1px solid #2f334d;
		border-radius: 6px;
		display: flex;
		align-items: center;
		overflow: hidden;
		transition: 0.2s;
	}
	.history-item:hover {
		transform: translateX(5px);
		border-color: #7aa2f7;
	}

	.hist-left-bar {
		width: 4px;
		background: #7aa2f7;
		align-self: stretch;
	}
	.hist-content {
		padding: 12px;
		flex: 1;
		text-align: left;
	}
	.hist-date {
		font-size: 0.75rem;
		color: #565f89;
		margin-bottom: 2px;
	}
	.hist-detail {
		font-weight: bold;
		font-size: 0.95rem;
		color: #c0caf5;
	}
	.hist-args {
		font-weight: normal;
		color: #7aa2f7;
		font-size: 0.8rem;
	}
	.hist-arrow {
		padding-right: 15px;
		opacity: 0;
		transition: 0.2s;
	}
	.history-item:hover .hist-arrow {
		opacity: 1;
	}

	/* Settings */
	.settings-form {
		max-width: 400px;
		display: flex;
		flex-direction: column;
		gap: 15px;
	}
	.settings-form input {
		background: #1a1b26;
		border: 1px solid #2f334d;
		border-radius: 6px;
	}
	.save-btn {
		background: #9ece6a;
		color: #1a1b26;
		padding: 10px;
	}

	@keyframes pulse {
		0% {
			opacity: 1;
		}
		50% {
			opacity: 0.7;
		}
		100% {
			opacity: 1;
		}
	}
	@keyframes fadeIn {
		from {
			opacity: 0;
			transform: translateX(-10px);
		}
		to {
			opacity: 1;
			transform: translateX(0);
		}
	}
</style>
