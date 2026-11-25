package dev.antigravity.mobile

import android.app.Activity
import android.app.ActivityManager
import android.app.AlertDialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.ComponentCallbacks2
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.graphics.Typeface
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.InputType
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.ViewGroup
import android.view.inputmethod.EditorInfo
import android.widget.Button
import android.widget.EditText
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import dev.antigravity.mobile.bridge.NativeBridge
import dev.antigravity.mobile.security.KeystoreVault
import dev.antigravity.mobile.ui.RenderedTerminalRow
import dev.antigravity.mobile.ui.TerminalLineAdapter
import dev.antigravity.mobile.ui.ThemePalette
import dev.antigravity.mobile.ui.UiMetrics
import dev.antigravity.mobile.workspace.SafWorkspaceSync
import org.json.JSONObject
import java.io.File
import java.util.concurrent.Executors

class MainActivity : Activity() {

    private enum class NavSection { TERMINAL, WORKSPACE, CONFIG }

    private val bgExecutor = Executors.newSingleThreadExecutor()
    private val uiHandler = Handler(Looper.getMainLooper())

    private var palette: ThemePalette = ThemePalette.DARK_HARDWARE
    private var activeSection: NavSection = NavSection.TERMINAL
    private var lastLineCount: Int = 0
    private var lastLastSeq: Long = -1L
    private var currentModeSlug: String = "standard"
    private var activeSafUri: Uri? = null
    private var activeWorkspaceFile: File? = null
    private var editingRelativePath: String? = null
    private var pollingActive: Boolean = false

    // Root & chrome views
    private lateinit var rootShell: LinearLayout
    private lateinit var topHeaderBar: LinearLayout
    private lateinit var runtimeBadgeLabel: TextView
    private lateinit var modeAndModelLabel: TextView
    private lateinit var themeToggleBtn: Button

    // Approval confirmation bar
    private lateinit var approvalBanner: LinearLayout
    private lateinit var approvalSummaryText: TextView
    private lateinit var approveBtn: Button
    private lateinit var denyBtn: Button

    // Responsive body containers
    private lateinit var middleArea: LinearLayout
    private lateinit var terminalPane: LinearLayout
    private lateinit var workspacePane: ScrollView
    private lateinit var configPane: ScrollView
    private lateinit var bottomNavBar: LinearLayout
    private lateinit var navRailBar: LinearLayout

    // Terminal widgets
    private lateinit var terminalStatusStrip: TextView
    private lateinit var terminalListView: ListView
    private lateinit var terminalAdapter: TerminalLineAdapter
    private lateinit var promptInput: EditText
    private lateinit var sendPromptBtn: Button
    private lateinit var planQuickBtn: Button

    // Workspace widgets
    private lateinit var wsPathText: TextView
    private lateinit var wsStateBanner: TextView
    private lateinit var wsSearchInput: EditText
    private lateinit var wsSearchResultsBox: LinearLayout
    private lateinit var wsGitSummaryText: TextView
    private lateinit var wsGitFilesBox: LinearLayout
    private lateinit var wsTreeBox: LinearLayout
    private lateinit var wsEditorContainer: LinearLayout
    private lateinit var wsEditorTitle: TextView
    private lateinit var wsEditorInput: EditText

    // Config widgets
    private lateinit var cfgCompatText: TextView
    private lateinit var cfgAuthStatusText: TextView
    private lateinit var cfgOauthClientIdInput: EditText
    private lateinit var cfgOauthCodeInput: EditText
    private lateinit var cfgApiKeyInput: EditText
    private lateinit var cfgCustomModelInput: EditText
    private lateinit var cfgModelsBox: LinearLayout
    private lateinit var cfgSessionsBox: LinearLayout
    private lateinit var installCliBtn: Button

    private val themedButtons = ArrayList<Pair<Button, Boolean>>()
    private val themedSurfaces = ArrayList<View>()
    private val themedPrimaryTexts = ArrayList<TextView>()
    private val themedSecondaryTexts = ArrayList<TextView>()
    private val themedInputs = ArrayList<EditText>()
    private val navButtons = HashMap<NavSection, Button>()

    private val pollRunnable = object : Runnable {
        override fun run() {
            if (!pollingActive) return
            refreshTerminalSnapshot()
            uiHandler.postDelayed(this, 350L)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        initializeNativeEngine()
        buildResponsiveUi()
        loadInitialConfigAndWorkspace()
    }

    override fun onResume() {
        super.onResume()
        pollingActive = true
        uiHandler.post(pollRunnable)
    }

    override fun onPause() {
        pollingActive = false
        uiHandler.removeCallbacks(pollRunnable)
        super.onPause()
    }

    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        if (level >= ComponentCallbacks2.TRIM_MEMORY_RUNNING_MODERATE) {
            try {
                NativeBridge.nativeOnTrimMemory(level)
            } catch (_: Throwable) {
            }
        }
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        val draftPrompt = if (::promptInput.isInitialized) promptInput.text.toString() else ""
        buildResponsiveUi()
        promptInput.setText(draftPrompt)
        refreshTerminalSnapshot(forceRedraw = true)
        if (activeSection == NavSection.WORKSPACE) refreshWorkspaceUi()
        if (activeSection == NavSection.CONFIG) refreshConfigUi()
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        if (event.action == KeyEvent.ACTION_DOWN) {
            if (event.isCtrlPressed && event.keyCode == KeyEvent.KEYCODE_C) {
                NativeBridge.nativeSendCtrlC()
                refreshTerminalSnapshot(forceRedraw = true)
                return true
            }
            if (event.isCtrlPressed && event.keyCode == KeyEvent.KEYCODE_D) {
                handleCtrlDAction()
                return true
            }
            if (promptInput.hasFocus()) {
                if (event.keyCode == KeyEvent.KEYCODE_DPAD_UP) {
                    navigateHistory(-1)
                    return true
                }
                if (event.keyCode == KeyEvent.KEYCODE_DPAD_DOWN) {
                    navigateHistory(1)
                    return true
                }
            }
            if (event.keyCode == KeyEvent.KEYCODE_ESCAPE && wsEditorContainer.visibility == View.VISIBLE) {
                closeInlineEditor()
                return true
            }
        }
        return super.dispatchKeyEvent(event)
    }

    @Deprecated("Deprecated in newer SDKs but required for API 21 compatibility without AndroidX ActivityResult")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQ_OPEN_SAF_TREE && resultCode == RESULT_OK) {
            val treeUri = data?.data ?: return
            val flags = data.flags and (Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            try {
                contentResolver.takePersistableUriPermission(treeUri, flags)
            } catch (_: Throwable) {
            }
            mountSafTreeAsync(treeUri)
        }
    }

    private fun initializeNativeEngine() {
        val am = getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager
        val memInfo = ActivityManager.MemoryInfo()
        am.getMemoryInfo(memInfo)
        val totalRamMb = (memInfo.totalMem / (1024L * 1024L)).coerceAtLeast(512L)

        val vault = KeystoreVault.loadOrCreateVaultKey(this)
        NativeBridge.nativeInit(
            filesDir.absolutePath,
            Build.VERSION.SDK_INT,
            totalRamMb,
            vault.keyBytes,
            vault.keystoreBacked
        )
    }

    private fun loadInitialConfigAndWorkspace() {
        val raw = NativeBridge.nativeGetFullStatus()
        val root = JSONObject(raw)
        if (!root.optBoolean("ok", false)) return
        val data = root.optJSONObject("data") ?: return
        val cfg = data.optJSONObject("config")
        if (cfg != null) {
            val themeStr = cfg.optString("theme_mode", "dark_hardware")
            palette = if (themeStr == "light_paper") ThemePalette.LIGHT_PAPER else ThemePalette.DARK_HARDWARE
            currentModeSlug = cfg.optString("agent_mode", "standard")
            val safStr = cfg.optString("active_saf_uri", "")
            if (safStr.isNotBlank() && safStr != "null") {
                activeSafUri = Uri.parse(safStr)
            }
            val wsStr = cfg.optString("active_workspace", "")
            if (wsStr.isNotBlank() && wsStr != "null") {
                activeWorkspaceFile = File(wsStr)
            }
        }
        applyPaletteToViews()
        refreshTerminalSnapshot(forceRedraw = true)
        refreshWorkspaceUi()
        refreshConfigUi()
    }

    private fun buildResponsiveUi() {
        themedButtons.clear()
        themedSurfaces.clear()
        themedPrimaryTexts.clear()
        themedSecondaryTexts.clear()
        themedInputs.clear()
        navButtons.clear()

        val widthDp = resources.configuration.screenWidthDp
        val isTabletRail = widthDp >= 600
        val isWideSplit = widthDp >= 840

        rootShell = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
        }

        // Top telemetry header
        topHeaderBar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            val p = dp(8)
            setPadding(dp(12), p, dp(12), p)
        }
        themedSurfaces.add(topHeaderBar)

        val titleColumn = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }

        runtimeBadgeLabel = TextView(this).apply {
            typeface = Typeface.create(Typeface.MONOSPACE, Typeface.BOLD)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            text = "AGY MOBILE | RUNTIME: CHECKING"
            maxLines = 1
            ellipsize = TextUtils.TruncateAt.END
        }
        modeAndModelLabel = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
            text = "MODE: STANDARD | gemini-2.5-flash"
            maxLines = 1
            ellipsize = TextUtils.TruncateAt.END
        }
        themedSecondaryTexts.add(modeAndModelLabel)

        titleColumn.addView(runtimeBadgeLabel)
        titleColumn.addView(modeAndModelLabel)

        themeToggleBtn = makeButton(if (palette.isDark) "Paper" else "Dark", primary = false) {
            toggleTheme()
        }.apply {
            contentDescription = "Switch between dark charcoal and light paper theme"
        }

        topHeaderBar.addView(titleColumn)
        topHeaderBar.addView(themeToggleBtn)
        rootShell.addView(topHeaderBar)
        rootShell.addView(makeHairline(2, isAccent = true))

        // Permission confirmation banner (hidden until pending_approval arrives)
        approvalBanner = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            val pad = dp(10)
            setPadding(pad, pad, pad, pad)
            visibility = View.GONE
        }
        themedSurfaces.add(approvalBanner)

        approvalSummaryText = TextView(this).apply {
            typeface = Typeface.create(Typeface.MONOSPACE, Typeface.BOLD)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
        }
        val approvalBtnRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(0, dp(6), 0, 0)
        }
        approveBtn = makeButton("Approve Action", primary = true) {
            NativeBridge.nativeResolveApproval(true)
            refreshTerminalSnapshot(forceRedraw = true)
        }
        denyBtn = makeButton("Deny", primary = false) {
            NativeBridge.nativeResolveApproval(false)
            refreshTerminalSnapshot(forceRedraw = true)
        }
        approvalBtnRow.addView(approveBtn)
        approvalBtnRow.addView(makeHorizontalSpacer(8))
        approvalBtnRow.addView(denyBtn)
        approvalBanner.addView(approvalSummaryText)
        approvalBanner.addView(approvalBtnRow)
        rootShell.addView(approvalBanner)

        // Build the 3 core content panes
        terminalPane = buildTerminalPane()
        workspacePane = buildWorkspacePane()
        configPane = buildConfigPane()

        middleArea = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                0,
                1f
            )
        }

        navRailBar = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            val pad = dp(8)
            setPadding(pad, pad, pad, pad)
            visibility = if (isTabletRail) View.VISIBLE else View.GONE
            layoutParams = LinearLayout.LayoutParams(dp(116), ViewGroup.LayoutParams.MATCH_PARENT)
        }
        themedSurfaces.add(navRailBar)

        bottomNavBar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            val pad = dp(6)
            setPadding(dp(8), pad, dp(8), pad)
            visibility = if (isTabletRail) View.GONE else View.VISIBLE
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT
            )
        }
        themedSurfaces.add(bottomNavBar)

        val destinations = listOf(
            NavSection.TERMINAL to "Terminal",
            NavSection.WORKSPACE to "Workspace",
            NavSection.CONFIG to "Config"
        )

        for ((idx, pair) in destinations.withIndex()) {
            val (sec, label) = pair
            val btn = makeButton(label, primary = (sec == activeSection)) {
                switchSection(sec)
            }.apply {
                contentDescription = "Open $label screen"
            }
            navButtons[sec] = btn

            if (isTabletRail) {
                btn.layoutParams = LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT,
                    ViewGroup.LayoutParams.WRAP_CONTENT
                ).apply {
                    if (idx > 0) topMargin = dp(8)
                }
                navRailBar.addView(btn)
            } else {
                btn.layoutParams = LinearLayout.LayoutParams(
                    0,
                    ViewGroup.LayoutParams.WRAP_CONTENT,
                    1f
                ).apply {
                    if (idx > 0) leftMargin = dp(8)
                }
                bottomNavBar.addView(btn)
            }
        }

        if (isTabletRail) {
            middleArea.addView(navRailBar)
        }

        if (isWideSplit) {
            // Wide landscape/tablet split: keep live Terminal alongside Workspace or Config
            terminalPane.layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 1.15f)
            val rightHost = LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 0.85f)
            }
            workspacePane.layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
            configPane.layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
            rightHost.addView(workspacePane)
            rightHost.addView(configPane)
            middleArea.addView(terminalPane)
            middleArea.addView(makeVerticalHairline())
            middleArea.addView(rightHost)
        } else {
            val contentStack = LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.MATCH_PARENT, 1f)
            }
            terminalPane.layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
            workspacePane.layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
            configPane.layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
            contentStack.addView(terminalPane)
            contentStack.addView(workspacePane)
            contentStack.addView(configPane)
            middleArea.addView(contentStack)
        }

        rootShell.addView(middleArea)
        if (!isTabletRail) {
            rootShell.addView(makeHairline(1, isAccent = false))
            rootShell.addView(bottomNavBar)
        }

        setContentView(rootShell)
        updateSectionVisibility()
        applyPaletteToViews()
    }

    private fun buildTerminalPane(): LinearLayout {
        val pane = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
        }

        terminalStatusStrip = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
            val pad = dp(6)
            setPadding(dp(12), pad, dp(12), pad)
            text = "Ready."
        }
        themedSecondaryTexts.add(terminalStatusStrip)
        pane.addView(terminalStatusStrip)
        pane.addView(makeHairline(1, isAccent = false))

        terminalAdapter = TerminalLineAdapter(this, palette)
        terminalListView = ListView(this).apply {
            adapter = terminalAdapter
            divider = null
            dividerHeight = 0
            transcriptMode = ListView.TRANSCRIPT_MODE_NORMAL
            isStackFromBottom = false
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                0,
                1f
            )
            setOnItemLongClickListener { _, _, position, _ ->
                val row = terminalAdapter.getItem(position) as RenderedTerminalRow
                copyToClipboard("Terminal line", row.plain)
                Toast.makeText(this@MainActivity, "Copied line to clipboard", Toast.LENGTH_SHORT).show()
                true
            }
        }
        pane.addView(terminalListView)
        pane.addView(makeHairline(1, isAccent = false))

        // Hardware control strip (scrollable horizontally so narrow screens never clip or crowd tap targets)
        val quickScroll = HorizontalScrollView(this).apply {
            isHorizontalScrollBarEnabled = false
        }
        themedSurfaces.add(quickScroll)

        val quickRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            val pad = dp(6)
            setPadding(dp(8), pad, dp(8), pad)
        }

        fun addQuick(label: String, desc: String, onClick: () -> Unit): Button {
            val b = makeButton(label, primary = false, compactHorizontal = true, onClick = onClick).apply {
                contentDescription = desc
            }
            if (quickRow.childCount > 0) {
                quickRow.addView(makeHorizontalSpacer(6))
            }
            quickRow.addView(b)
            return b
        }

        addQuick("Ctrl+C", "Interrupt active command or stream") {
            NativeBridge.nativeSendCtrlC()
            refreshTerminalSnapshot(forceRedraw = true)
        }
        addQuick("Ctrl+D", "Send EOF or close session") {
            handleCtrlDAction()
        }
        addQuick("Hist Up", "Previous command in history") {
            navigateHistory(-1)
        }
        addQuick("Hist Dn", "Next command in history") {
            navigateHistory(1)
        }
        planQuickBtn = addQuick("Mode: Std", "Toggle between Standard and Plan mode") {
            val next = if (currentModeSlug == "plan") "standard" else "plan"
            NativeBridge.nativeUpdateConfig("", next, "", "")
            currentModeSlug = next
            NativeBridge.nativeSubmitInput("/mode $next")
            refreshTerminalSnapshot(forceRedraw = true)
        }
        addQuick("Copy All", "Copy terminal transcript to clipboard") {
            val resp = JSONObject(NativeBridge.nativeExportTerminalText())
            val txt = resp.optString("data", "")
            copyToClipboard("AGY Terminal Transcript", txt)
            Toast.makeText(this, "Terminal output copied", Toast.LENGTH_SHORT).show()
        }
        addQuick("Paste", "Paste clipboard text into command input") {
            pasteFromClipboard()?.let { clip ->
                val cur = promptInput.text.toString()
                promptInput.setText(cur + clip)
                promptInput.setSelection(promptInput.text.length)
            }
        }
        addQuick("Clear", "Clear terminal scrollback") {
            NativeBridge.nativeSubmitInput("/clear")
            refreshTerminalSnapshot(forceRedraw = true)
        }

        quickScroll.addView(quickRow)
        pane.addView(quickScroll)

        // Command input bar with visible label
        val inputBar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            val pad = dp(8)
            setPadding(pad, dp(4), pad, pad)
        }
        themedSurfaces.add(inputBar)

        val promptId = View.generateViewId()
        val promptLabel = TextView(this).apply {
            typeface = Typeface.create(Typeface.MONOSPACE, Typeface.BOLD)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
            text = "agy>"
            labelFor = promptId
            setPadding(dp(4), 0, dp(8), 0)
        }
        themedPrimaryTexts.add(promptLabel)

        promptInput = EditText(this).apply {
            id = promptId
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
            hint = "Enter prompt or /help"
            isSingleLine = true
            imeOptions = EditorInfo.IME_ACTION_SEND
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            minHeight = dp(48)
            setPadding(dp(10), dp(8), dp(10), dp(8))
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
            setOnEditorActionListener { _, actionId, _ ->
                if (actionId == EditorInfo.IME_ACTION_SEND || actionId == EditorInfo.IME_ACTION_DONE) {
                    submitPromptInput()
                    true
                } else {
                    false
                }
            }
        }
        themedInputs.add(promptInput)

        sendPromptBtn = makeButton("Run", primary = true) {
            submitPromptInput()
        }.apply {
            contentDescription = "Run terminal command or agent prompt"
        }

        inputBar.addView(promptLabel)
        inputBar.addView(promptInput)
        inputBar.addView(makeHorizontalSpacer(8))
        inputBar.addView(sendPromptBtn)
        pane.addView(inputBar)

        return pane
    }

    private fun buildWorkspacePane(): ScrollView {
        val scroll = ScrollView(this).apply {
            isFillViewport = true
        }
        val body = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            val pad = dp(12)
            setPadding(pad, pad, pad, dp(24))
        }

        body.addView(makeSectionHeading("Workspace & Storage Access Framework"))
        wsPathText = makeBodyText("Active path: loading...", monospace = true)
        body.addView(wsPathText)

        wsStateBanner = makeBodyText("", monospace = false).apply {
            visibility = View.GONE
            setPadding(0, dp(4), 0, dp(4))
        }
        body.addView(wsStateBanner)

        val actionRow1 = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(0, dp(8), 0, dp(8))
        }
        val safBtn = makeButton("Open SAF Folder", primary = true) {
            launchSafFolderPicker()
        }.apply {
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        val localProjBtn = makeButton("New Project", primary = false) {
            promptCreateLocalProject()
        }.apply {
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                leftMargin = dp(8)
            }
        }
        actionRow1.addView(safBtn)
        actionRow1.addView(localProjBtn)
        body.addView(actionRow1)

        val actionRow2 = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(0, 0, 0, dp(12))
        }
        val newFileBtn = makeButton("New File", primary = false) {
            promptCreateNewFile()
        }.apply {
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        val refreshWsBtn = makeButton("Refresh Tree & Git", primary = false) {
            refreshWorkspaceUi()
        }.apply {
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                leftMargin = dp(8)
            }
        }
        actionRow2.addView(newFileBtn)
        actionRow2.addView(refreshWsBtn)
        body.addView(actionRow2)

        // Inline File Editor Card (hidden until a file is opened or created)
        wsEditorContainer = makeCardContainer().apply {
            visibility = View.GONE
        }
        wsEditorTitle = makeSectionHeading("Editing file")
        val editorId = View.generateViewId()
        val editorLabel = makeBodyText("File contents (UTF-8):", monospace = false).apply {
            labelFor = editorId
        }
        wsEditorInput = EditText(this).apply {
            id = editorId
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            gravity = Gravity.TOP or Gravity.START
            minLines = 8
            maxLines = 18
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            setPadding(dp(10), dp(10), dp(10), dp(10))
        }
        themedInputs.add(wsEditorInput)

        val editorBtns = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(0, dp(8), 0, 0)
        }
        val saveFileBtn = makeButton("Save File", primary = true) {
            confirmAndSaveEditorFile()
        }
        val deleteFileBtn = makeButton("Delete", primary = false) {
            confirmAndDeleteEditorFile()
        }
        val closeEditorBtn = makeButton("Close", primary = false) {
            closeInlineEditor()
        }
        editorBtns.addView(saveFileBtn)
        editorBtns.addView(makeHorizontalSpacer(8))
        editorBtns.addView(deleteFileBtn)
        editorBtns.addView(makeHorizontalSpacer(8))
        editorBtns.addView(closeEditorBtn)

        wsEditorContainer.addView(wsEditorTitle)
        wsEditorContainer.addView(editorLabel)
        wsEditorContainer.addView(wsEditorInput)
        wsEditorContainer.addView(editorBtns)
        body.addView(wsEditorContainer)

        // Streaming file search
        val searchCard = makeCardContainer()
        val searchId = View.generateViewId()
        val searchLabel = makeSectionHeading("Search Workspace Files").apply {
            labelFor = searchId
        }
        val searchRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(0, dp(6), 0, dp(6))
        }
        wsSearchInput = EditText(this).apply {
            id = searchId
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            hint = "Function, symbol, or text"
            isSingleLine = true
            minHeight = dp(48)
            setPadding(dp(10), dp(8), dp(10), dp(8))
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        themedInputs.add(wsSearchInput)
        val searchBtn = makeButton("Find", primary = false) {
            runWorkspaceSearch()
        }
        searchRow.addView(wsSearchInput)
        searchRow.addView(makeHorizontalSpacer(8))
        searchRow.addView(searchBtn)

        wsSearchResultsBox = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
        }
        searchCard.addView(searchLabel)
        searchCard.addView(searchRow)
        searchCard.addView(wsSearchResultsBox)
        body.addView(searchCard)

        // Native Git inspector card
        val gitCard = makeCardContainer()
        gitCard.addView(makeSectionHeading("Git Repository Inspector"))
        wsGitSummaryText = makeBodyText("Checking .git directory...", monospace = true)
        wsGitFilesBox = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, dp(6), 0, 0)
        }
        gitCard.addView(wsGitSummaryText)
        gitCard.addView(wsGitFilesBox)
        body.addView(gitCard)

        // Project file tree card
        val treeCard = makeCardContainer()
        treeCard.addView(makeSectionHeading("Project Tree (Tap a file to inspect or edit)"))
        wsTreeBox = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, dp(6), 0, 0)
        }
        treeCard.addView(wsTreeBox)
        body.addView(treeCard)

        scroll.addView(body)
        return scroll
    }

    private fun buildConfigPane(): ScrollView {
        val scroll = ScrollView(this).apply {
            isFillViewport = true
        }
        val body = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            val pad = dp(12)
            setPadding(pad, pad, pad, dp(24))
        }

        // 1. Runtime & Architecture Honesty Card
        val compatCard = makeCardContainer()
        compatCard.addView(makeSectionHeading("Architecture & Runtime Mode"))
        cfgCompatText = makeBodyText("Loading hardware compatibility report...", monospace = true)
        compatCard.addView(cfgCompatText)

        val rtRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(0, dp(8), 0, 0)
        }
        val rtAutoBtn = makeButton("Auto Runtime", primary = false) {
            NativeBridge.nativeUpdateConfig("", "", "auto", "")
            refreshConfigUi()
            refreshTerminalSnapshot(forceRedraw = true)
        }.apply {
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        val rtApiBtn = makeButton("Force API-Compat", primary = false) {
            NativeBridge.nativeUpdateConfig("", "", "force_compatible_api", "")
            refreshConfigUi()
            refreshTerminalSnapshot(forceRedraw = true)
        }.apply {
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                leftMargin = dp(8)
            }
        }
        rtRow.addView(rtAutoBtn)
        rtRow.addView(rtApiBtn)
        compatCard.addView(rtRow)

        installCliBtn = makeButton("Download Official Google ARM64 CLI (SHA-512 verified)", primary = false) {
            NativeBridge.nativeInstallOfficialCli()
            switchSection(NavSection.TERMINAL)
        }.apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT
            ).apply { topMargin = dp(8) }
        }
        compatCard.addView(installCliBtn)
        body.addView(compatCard)

        // 2. Google OAuth PKCE + Keystore API Key Card
        val authCard = makeCardContainer()
        authCard.addView(makeSectionHeading("Authentication (Android Keystore Protected)"))
        cfgAuthStatusText = makeBodyText("Status: checking...", monospace = true)
        authCard.addView(cfgAuthStatusText)

        val cidId = View.generateViewId()
        val cidLabel = makeBodyText("Google Cloud OAuth 2.0 Client ID (PKCE S256, no secret):", monospace = false).apply {
            labelFor = cidId
            setPadding(0, dp(8), 0, dp(2))
        }
        cfgOauthClientIdInput = EditText(this).apply {
            id = cidId
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            hint = "123456789-abc.apps.googleusercontent.com"
            isSingleLine = true
            minHeight = dp(48)
            setPadding(dp(10), dp(8), dp(10), dp(8))
        }
        themedInputs.add(cfgOauthClientIdInput)

        val startOauthBtn = makeButton("Sign In with Google Browser OAuth (PKCE)", primary = true) {
            startBrowserOauthFlow()
        }.apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT
            ).apply { topMargin = dp(6) }
        }

        val codeId = View.generateViewId()
        val codeLabel = makeBodyText("Redirect URL or authorization code (if browser did not loopback):", monospace = false).apply {
            labelFor = codeId
            setPadding(0, dp(8), 0, dp(2))
        }
        cfgOauthCodeInput = EditText(this).apply {
            id = codeId
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            hint = "http://127.0.0.1:.../oauth2callback?code=...&state=..."
            isSingleLine = true
            minHeight = dp(48)
            setPadding(dp(10), dp(8), dp(10), dp(8))
        }
        themedInputs.add(cfgOauthCodeInput)

        val finishOauthBtn = makeButton("Exchange Authorization Code", primary = false) {
            completeManualOauthCode()
        }.apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT
            ).apply { topMargin = dp(6) }
        }

        val apiKeyId = View.generateViewId()
        val apiKeyLabel = makeBodyText("Or authorized Gemini API Key (sent via x-goog-api-key header):", monospace = false).apply {
            labelFor = apiKeyId
            setPadding(0, dp(12), 0, dp(2))
        }
        cfgApiKeyInput = EditText(this).apply {
            id = apiKeyId
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            hint = "Enter Gemini API key"
            isSingleLine = true
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            minHeight = dp(48)
            setPadding(dp(10), dp(8), dp(10), dp(8))
        }
        themedInputs.add(cfgApiKeyInput)

        val keyBtnRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(0, dp(6), 0, 0)
        }
        val saveKeyBtn = makeButton("Save Key", primary = true) {
            val rawKey = cfgApiKeyInput.text.toString().trim()
            val resp = JSONObject(NativeBridge.nativeSaveApiKey(rawKey))
            if (resp.optBoolean("ok", false)) {
                cfgApiKeyInput.setText("")
                Toast.makeText(this, "Credential encrypted in Keystore vault", Toast.LENGTH_SHORT).show()
                refreshConfigUi()
            } else {
                Toast.makeText(this, resp.optString("error", "Failed"), Toast.LENGTH_LONG).show()
            }
        }.apply {
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        val signOutBtn = makeButton("Sign Out", primary = false) {
            NativeBridge.nativeSignOut()
            refreshConfigUi()
            Toast.makeText(this, "Stored credentials wiped", Toast.LENGTH_SHORT).show()
        }.apply {
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                leftMargin = dp(8)
            }
        }
        keyBtnRow.addView(saveKeyBtn)
        keyBtnRow.addView(signOutBtn)

        authCard.addView(cidLabel)
        authCard.addView(cfgOauthClientIdInput)
        authCard.addView(startOauthBtn)
        authCard.addView(codeLabel)
        authCard.addView(cfgOauthCodeInput)
        authCard.addView(finishOauthBtn)
        authCard.addView(apiKeyLabel)
        authCard.addView(cfgApiKeyInput)
        authCard.addView(keyBtnRow)
        body.addView(authCard)

        // 3. Model & Execution Mode Card
        val modelCard = makeCardContainer()
        modelCard.addView(makeSectionHeading("Execution Mode & Model Selection"))
        val modeBtnRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(0, dp(6), 0, dp(8))
        }
        for ((idx, pair) in listOf("standard" to "Standard", "plan" to "Plan Mode", "read_only" to "Read-Only").withIndex()) {
            val (slug, title) = pair
            val b = makeButton(title, primary = false) {
                currentModeSlug = slug
                NativeBridge.nativeUpdateConfig("", slug, "", "")
                refreshConfigUi()
                refreshTerminalSnapshot(forceRedraw = true)
            }.apply {
                layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                    if (idx > 0) leftMargin = dp(6)
                }
            }
            modeBtnRow.addView(b)
        }
        modelCard.addView(modeBtnRow)

        val customModelId = View.generateViewId()
        val customModelLabel = makeBodyText("Model ID:", monospace = false).apply {
            labelFor = customModelId
        }
        val customModelRow = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(0, dp(4), 0, dp(8))
        }
        cfgCustomModelInput = EditText(this).apply {
            id = customModelId
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            hint = "gemini-2.5-flash"
            isSingleLine = true
            minHeight = dp(48)
            setPadding(dp(10), dp(8), dp(10), dp(8))
            layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
        }
        themedInputs.add(cfgCustomModelInput)
        val applyModelBtn = makeButton("Select", primary = false) {
            val m = cfgCustomModelInput.text.toString().trim()
            if (m.isNotEmpty()) {
                NativeBridge.nativeUpdateConfig(m, "", "", "")
                refreshConfigUi()
                refreshTerminalSnapshot(forceRedraw = true)
            }
        }
        customModelRow.addView(cfgCustomModelInput)
        customModelRow.addView(makeHorizontalSpacer(8))
        customModelRow.addView(applyModelBtn)

        val fetchModelsBtn = makeButton("Query Models from Active Runtime", primary = false) {
            NativeBridge.nativeRefreshModels()
            uiHandler.postDelayed({ refreshConfigUi() }, 900L)
        }.apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT
            )
        }

        cfgModelsBox = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, dp(8), 0, 0)
        }

        modelCard.addView(customModelLabel)
        modelCard.addView(customModelRow)
        modelCard.addView(fetchModelsBtn)
        modelCard.addView(cfgModelsBox)
        body.addView(modelCard)

        // 4. Persistent Sessions Card
        val sessionsCard = makeCardContainer()
        sessionsCard.addView(makeSectionHeading("Persistent Sessions"))
        val newSessBtn = makeButton("Start New Session", primary = false) {
            NativeBridge.nativeSessionAction("new", "Session")
            refreshConfigUi()
            refreshTerminalSnapshot(forceRedraw = true)
        }.apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT
            ).apply { topMargin = dp(6) }
        }
        cfgSessionsBox = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, dp(8), 0, 0)
        }
        sessionsCard.addView(newSessBtn)
        sessionsCard.addView(cfgSessionsBox)
        body.addView(sessionsCard)

        scroll.addView(body)
        return scroll
    }

    private fun switchSection(section: NavSection) {
        activeSection = section
        updateSectionVisibility()
        applyPaletteToViews()
        if (section == NavSection.WORKSPACE) refreshWorkspaceUi()
        if (section == NavSection.CONFIG) refreshConfigUi()
    }

    private fun updateSectionVisibility() {
        val isWideSplit = resources.configuration.screenWidthDp >= 840
        if (isWideSplit) {
            terminalPane.visibility = View.VISIBLE
            if (activeSection == NavSection.CONFIG) {
                workspacePane.visibility = View.GONE
                configPane.visibility = View.VISIBLE
            } else {
                workspacePane.visibility = View.VISIBLE
                configPane.visibility = View.GONE
            }
        } else {
            terminalPane.visibility = if (activeSection == NavSection.TERMINAL) View.VISIBLE else View.GONE
            workspacePane.visibility = if (activeSection == NavSection.WORKSPACE) View.VISIBLE else View.GONE
            configPane.visibility = if (activeSection == NavSection.CONFIG) View.VISIBLE else View.GONE
        }

        for ((sec, btn) in navButtons) {
            val selected = (sec == activeSection)
            btn.background = UiMetrics.interactiveButtonBackground(this, palette, selected)
            btn.setTextColor(UiMetrics.buttonTextColors(palette, selected))
        }
    }

    private fun toggleTheme() {
        palette = if (palette.isDark) ThemePalette.LIGHT_PAPER else ThemePalette.DARK_HARDWARE
        val slug = if (palette.isDark) "dark_hardware" else "light_paper"
        NativeBridge.nativeUpdateConfig("", "", "", slug)
        themeToggleBtn.text = if (palette.isDark) "Paper" else "Dark"
        applyPaletteToViews()
    }

    private fun applyPaletteToViews() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP) {
            window.statusBarColor = palette.surfaceBg
            window.navigationBarColor = palette.surfaceBg
        }
        rootShell.setBackgroundColor(palette.canvasBg)
        terminalListView.setBackgroundColor(palette.canvasBg)
        workspacePane.setBackgroundColor(palette.canvasBg)
        configPane.setBackgroundColor(palette.canvasBg)

        runtimeBadgeLabel.setTextColor(palette.accent)
        approvalSummaryText.setTextColor(palette.errorText)

        for (surface in themedSurfaces) {
            if (surface === topHeaderBar || surface === bottomNavBar || surface === navRailBar) {
                surface.setBackgroundColor(palette.surfaceBg)
            } else {
                surface.background = UiMetrics.borderedSurface(this, palette, 4)
            }
        }

        for (tv in themedPrimaryTexts) {
            tv.setTextColor(palette.primaryText)
        }
        for (tv in themedSecondaryTexts) {
            tv.setTextColor(palette.secondaryText)
        }
        for (input in themedInputs) {
            input.background = UiMetrics.borderedSurface(this, palette, 4)
            input.setTextColor(palette.primaryText)
            input.setHintTextColor(palette.secondaryText)
        }
        for ((btn, primary) in themedButtons) {
            btn.background = UiMetrics.interactiveButtonBackground(this, palette, primary)
            btn.setTextColor(UiMetrics.buttonTextColors(palette, primary))
        }
        for ((sec, btn) in navButtons) {
            val active = (sec == activeSection)
            btn.background = UiMetrics.interactiveButtonBackground(this, palette, active)
            btn.setTextColor(UiMetrics.buttonTextColors(palette, active))
        }

        terminalAdapter.updatePalette(palette)
    }

    private fun submitPromptInput() {
        val raw = promptInput.text.toString()
        if (raw.trim().isEmpty()) return
        promptInput.setText("")
        NativeBridge.nativeSubmitInput(raw)
        refreshTerminalSnapshot(forceRedraw = true)
    }

    private fun handleCtrlDAction() {
        val empty = promptInput.text.toString().isEmpty()
        if (!empty) {
            promptInput.setText("")
        }
        NativeBridge.nativeSendCtrlD(empty)
        refreshTerminalSnapshot(forceRedraw = true)
    }

    private fun navigateHistory(direction: Int) {
        val resp = JSONObject(NativeBridge.nativeHistoryNav(direction))
        if (!resp.optBoolean("ok", false)) return
        val data = resp.optJSONObject("data") ?: return
        if (data.isNull("entry")) {
            if (direction > 0) promptInput.setText("")
        } else {
            val cmd = data.optString("entry", "")
            promptInput.setText(cmd)
            promptInput.setSelection(cmd.length)
        }
    }

    private fun refreshTerminalSnapshot(forceRedraw: Boolean = false) {
        val raw = try {
            NativeBridge.nativeGetTerminalSnapshot()
        } catch (_: Throwable) {
            return
        }
        val root = JSONObject(raw)
        if (!root.optBoolean("ok", false)) return
        val snap = root.optJSONObject("data") ?: return

        val badge = snap.optString("runtime_badge", "RUNTIME: API-COMPAT")
        val modeLbl = snap.optString("mode_label", "STANDARD")
        val model = snap.optString("model", "gemini-2.5-flash")
        val sessTitle = snap.optString("session_title", "Session")
        val busy = snap.optBoolean("busy", false)
        val statusLine = snap.optString("status_line", "Ready.")

        runtimeBadgeLabel.text = "AGY MOBILE | $badge"
        modeAndModelLabel.text = "MODE: $modeLbl | $model | $sessTitle"
        terminalStatusStrip.text = if (busy) "[WORKING] $statusLine" else statusLine
        planQuickBtn.text = if (modeLbl == "PLAN") "Mode: Plan" else "Mode: Std"

        val pending = snap.optJSONObject("pending_approval")
        if (pending != null) {
            approvalBanner.visibility = View.VISIBLE
            approvalSummaryText.text = "[APPROVAL REQUIRED] ${pending.optString("summary", "")}"
        } else {
            approvalBanner.visibility = View.GONE
        }

        val dirtySaf = snap.optJSONArray("dirty_saf_paths")
        if (dirtySaf != null && dirtySaf.length() > 0) {
            val uri = activeSafUri
            val localDir = activeWorkspaceFile
            if (uri != null && localDir != null) {
                val paths = ArrayList<String>(dirtySaf.length())
                for (i in 0 until dirtySaf.length()) {
                    paths.add(dirtySaf.optString(i))
                }
                bgExecutor.execute {
                    SafWorkspaceSync.flushDirtyPathsToSaf(applicationContext, uri, localDir, paths)
                }
            }
        }

        val linesArr = snap.optJSONArray("lines") ?: return
        val count = linesArr.length()
        val lastSeq = if (count > 0) {
            linesArr.optJSONObject(count - 1)?.optLong("seq", -1L) ?: -1L
        } else {
            0L
        }

        if (forceRedraw || count != lastLineCount || lastSeq != lastLastSeq) {
            val wasNearBottom = terminalListView.lastVisiblePosition >= (terminalAdapter.count - 3).coerceAtLeast(0)
            lastLineCount = count
            lastLastSeq = lastSeq
            terminalAdapter.submitLinesJson(linesArr)
            if (wasNearBottom && terminalAdapter.count > 0) {
                terminalListView.setSelection(terminalAdapter.count - 1)
            }
        }
    }

    private fun refreshWorkspaceUi() {
        val resp = JSONObject(NativeBridge.nativeWorkspaceOverview())
        if (!resp.optBoolean("ok", false)) {
            wsStateBanner.visibility = View.VISIBLE
            wsStateBanner.setTextColor(palette.errorText)
            wsStateBanner.text = "[ERROR] ${resp.optString("error", "Failed to read workspace")}"
            return
        }
        val data = resp.optJSONObject("data") ?: return
        val rootPath = data.optString("root_path", "")
        val safUri = data.optString("saf_uri", "")
        activeWorkspaceFile = if (rootPath.isNotEmpty()) File(rootPath) else null
        wsPathText.text = if (safUri.isNotBlank() && safUri != "null") {
            "SAF URI: $safUri\nMirror : $rootPath"
        } else {
            "Local workspace: $rootPath"
        }

        // Populate Git Status & Log
        wsGitFilesBox.removeAllViews()
        val git = data.optJSONObject("git")
        if (git == null || !git.optBoolean("is_repo", false)) {
            wsGitSummaryText.text = "No .git repository in this workspace. Open a Git folder via SAF or run `!git init`."
        } else {
            val branch = git.optString("branch", "main")
            val head = git.optString("head_commit", "").take(7).ifEmpty { "unborn" }
            val backend = git.optString("backend", "rust-native-git")
            val changes = git.optJSONArray("changes")
            val changeCount = changes?.length() ?: 0
            wsGitSummaryText.text = "Branch: $branch ($head) | $changeCount changed | [$backend]"

            if (changes != null && changeCount > 0) {
                for (i in 0 until changeCount) {
                    val ch = changes.optJSONObject(i) ?: continue
                    val st = ch.optString("status", "M")
                    val path = ch.optString("path", "")
                    val row = LinearLayout(this).apply {
                        orientation = LinearLayout.HORIZONTAL
                        gravity = Gravity.CENTER_VERTICAL
                        setPadding(0, dp(3), 0, dp(3))
                    }
                    val lbl = makeBodyText("[$st] $path", monospace = true).apply {
                        layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
                    }
                    val diffBtn = makeButton("Diff", primary = false, compactHorizontal = true) {
                        showGitDiffDialog(path)
                    }
                    row.addView(lbl)
                    row.addView(diffBtn)
                    wsGitFilesBox.addView(row)
                }
            }
            val commits = data.optJSONArray("commits")
            if (commits != null && commits.length() > 0) {
                wsGitFilesBox.addView(makeBodyText("Recent commits:", monospace = false).apply {
                    setPadding(0, dp(6), 0, dp(2))
                })
                for (i in 0 until commits.length().coerceAtMost(5)) {
                    val c = commits.optJSONObject(i) ?: continue
                    wsGitFilesBox.addView(
                        makeBodyText(
                            "${c.optString("short_id")} ${c.optString("summary")} (${c.optString("author")})",
                            monospace = true
                        )
                    )
                }
            }
        }

        // Populate Project Tree
        wsTreeBox.removeAllViews()
        val entries = data.optJSONArray("entries")
        if (entries == null || entries.length() == 0) {
            wsTreeBox.addView(
                makeBodyText(
                    "Workspace folder is empty. Tap 'New File' above to create a file or 'Open SAF Folder' to import an existing project.",
                    monospace = false
                )
            )
        } else {
            for (i in 0 until entries.length()) {
                val e = entries.optJSONObject(i) ?: continue
                val rel = e.optString("relative_path", "")
                val name = e.optString("name", "")
                val isDir = e.optBoolean("is_dir", false)
                val depth = e.optInt("depth", 0)
                val size = e.optLong("size_bytes", 0L)
                val prefix = "  ".repeat(depth) + if (isDir) "[DIR] " else "      "
                val suffix = if (isDir) "/" else " (${size}B)"

                if (isDir) {
                    wsTreeBox.addView(makeBodyText("$prefix$name$suffix", monospace = true))
                } else {
                    val fileBtn = makeButton("$prefix$name$suffix", primary = false) {
                        openFileInEditor(rel)
                    }.apply {
                        gravity = Gravity.START or Gravity.CENTER_VERTICAL
                        typeface = Typeface.MONOSPACE
                        layoutParams = LinearLayout.LayoutParams(
                            ViewGroup.LayoutParams.MATCH_PARENT,
                            ViewGroup.LayoutParams.WRAP_CONTENT
                        ).apply { topMargin = dp(4) }
                    }
                    wsTreeBox.addView(fileBtn)
                }
            }
        }
        applyPaletteToViews()
    }

    private fun openFileInEditor(relPath: String) {
        val resp = JSONObject(NativeBridge.nativeWorkspaceReadFile(relPath))
        if (!resp.optBoolean("ok", false)) {
            Toast.makeText(this, resp.optString("error", "Cannot read file"), Toast.LENGTH_LONG).show()
            return
        }
        editingRelativePath = relPath
        wsEditorTitle.text = "Editing: $relPath"
        wsEditorInput.setText(resp.optString("data", ""))
        wsEditorContainer.visibility = View.VISIBLE
        workspacePane.post { workspacePane.smoothScrollTo(0, wsEditorContainer.top) }
    }

    private fun closeInlineEditor() {
        editingRelativePath = null
        wsEditorContainer.visibility = View.GONE
    }

    private fun confirmAndSaveEditorFile() {
        val path = editingRelativePath ?: return
        val content = wsEditorInput.text.toString()
        AlertDialog.Builder(this)
            .setTitle("Confirm file write")
            .setMessage("Write ${content.toByteArray().size} bytes to '$path'?")
            .setPositiveButton("Save") { _, _ ->
                val resp = JSONObject(NativeBridge.nativeWorkspaceSaveFile(path, content, true))
                if (resp.optBoolean("ok", false)) {
                    activeSafUri?.let { uri ->
                        activeWorkspaceFile?.let { root ->
                            bgExecutor.execute {
                                SafWorkspaceSync.flushDirtyPathsToSaf(applicationContext, uri, root, listOf(path))
                            }
                        }
                    }
                    Toast.makeText(this, "Saved $path", Toast.LENGTH_SHORT).show()
                    refreshWorkspaceUi()
                } else {
                    Toast.makeText(this, resp.optString("error", "Write failed"), Toast.LENGTH_LONG).show()
                }
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun confirmAndDeleteEditorFile() {
        val path = editingRelativePath ?: return
        AlertDialog.Builder(this)
            .setTitle("Confirm destructive delete")
            .setMessage("Permanently delete '$path' from workspace?")
            .setPositiveButton("Delete") { _, _ ->
                val resp = JSONObject(NativeBridge.nativeWorkspaceDeleteFile(path, true))
                if (resp.optBoolean("ok", false)) {
                    activeSafUri?.let { uri ->
                        activeWorkspaceFile?.let { root ->
                            bgExecutor.execute {
                                SafWorkspaceSync.flushDirtyPathsToSaf(
                                    applicationContext,
                                    uri,
                                    root,
                                    listOf("DELETED:$path")
                                )
                            }
                        }
                    }
                    closeInlineEditor()
                    refreshWorkspaceUi()
                    Toast.makeText(this, "Deleted $path", Toast.LENGTH_SHORT).show()
                } else {
                    Toast.makeText(this, resp.optString("error", "Delete failed"), Toast.LENGTH_LONG).show()
                }
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun runWorkspaceSearch() {
        val q = wsSearchInput.text.toString().trim()
        wsSearchResultsBox.removeAllViews()
        if (q.isEmpty()) {
            wsSearchResultsBox.addView(makeBodyText("Enter a search term above.", monospace = false))
            applyPaletteToViews()
            return
        }
        val resp = JSONObject(NativeBridge.nativeWorkspaceSearch(q))
        if (!resp.optBoolean("ok", false)) {
            wsSearchResultsBox.addView(makeBodyText("[ERROR] ${resp.optString("error")}", monospace = false))
            applyPaletteToViews()
            return
        }
        val arr = resp.optJSONArray("data")
        if (arr == null || arr.length() == 0) {
            wsSearchResultsBox.addView(makeBodyText("No matches found for '$q'.", monospace = false))
        } else {
            for (i in 0 until arr.length()) {
                val hit = arr.optJSONObject(i) ?: continue
                val rel = hit.optString("relative_path")
                val line = hit.optInt("line_number")
                val prev = hit.optString("preview")
                val btn = makeButton("$rel:$line  $prev", primary = false) {
                    openFileInEditor(rel)
                }.apply {
                    gravity = Gravity.START or Gravity.CENTER_VERTICAL
                    typeface = Typeface.MONOSPACE
                    layoutParams = LinearLayout.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.WRAP_CONTENT
                    ).apply { topMargin = dp(4) }
                }
                wsSearchResultsBox.addView(btn)
            }
        }
        applyPaletteToViews()
    }

    private fun showGitDiffDialog(relPath: String) {
        val resp = JSONObject(NativeBridge.nativeWorkspaceGitDiff(relPath))
        val diff = if (resp.optBoolean("ok", false)) {
            resp.optString("data", "No changes.")
        } else {
            resp.optString("error", "Failed to compute diff.")
        }
        AlertDialog.Builder(this)
            .setTitle("Git Diff: $relPath")
            .setMessage(diff)
            .setPositiveButton("Close", null)
            .show()
    }

    private fun launchSafFolderPicker() {
        try {
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
                addFlags(Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
            }
            @Suppress("DEPRECATION")
            startActivityForResult(intent, REQ_OPEN_SAF_TREE)
        } catch (e: Throwable) {
            wsStateBanner.visibility = View.VISIBLE
            wsStateBanner.setTextColor(palette.errorText)
            wsStateBanner.text =
                "[SAF UNAVAILABLE] Device lacks DocumentsUI picker (${e.message}). Use 'New Project' to create a local workspace."
        }
    }

    private fun mountSafTreeAsync(treeUri: Uri) {
        wsStateBanner.visibility = View.VISIBLE
        wsStateBanner.setTextColor(palette.accent)
        wsStateBanner.text = "[LOADING] Streaming SAF folder tree into local mirror..."
        bgExecutor.execute {
            try {
                val mirrored = SafWorkspaceSync.mirrorTreeFromSaf(applicationContext, treeUri)
                NativeBridge.nativeSetWorkspace(mirrored.absolutePath, treeUri.toString())
                uiHandler.post {
                    activeSafUri = treeUri
                    activeWorkspaceFile = mirrored
                    wsStateBanner.visibility = View.GONE
                    refreshWorkspaceUi()
                    refreshTerminalSnapshot(forceRedraw = true)
                }
            } catch (t: Throwable) {
                uiHandler.post {
                    wsStateBanner.visibility = View.VISIBLE
                    wsStateBanner.setTextColor(palette.errorText)
                    wsStateBanner.text = "[ERROR] Failed to mirror SAF tree: ${t.message}"
                }
            }
        }
    }

    private fun promptCreateLocalProject() {
        val input = EditText(this).apply {
            hint = "project-name"
            isSingleLine = true
        }
        AlertDialog.Builder(this)
            .setTitle("Create or Mount Local Project")
            .setView(input)
            .setPositiveButton("Open") { _, _ ->
                val rawName = input.text.toString().trim()
                val safeName = rawName.replace(Regex("[^a-zA-Z0-9._-]"), "-").ifEmpty { "project" }
                val dir = File(filesDir, "workspaces/$safeName")
                dir.mkdirs()
                activeSafUri = null
                activeWorkspaceFile = dir
                NativeBridge.nativeSetWorkspace(dir.absolutePath, "")
                refreshWorkspaceUi()
                refreshTerminalSnapshot(forceRedraw = true)
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun promptCreateNewFile() {
        val input = EditText(this).apply {
            hint = "src/main.rs"
            isSingleLine = true
        }
        AlertDialog.Builder(this)
            .setTitle("New Workspace File")
            .setView(input)
            .setPositiveButton("Create") { _, _ ->
                val rel = input.text.toString().trim()
                if (rel.isNotEmpty()) {
                    val resp = JSONObject(NativeBridge.nativeWorkspaceSaveFile(rel, "", true))
                    if (resp.optBoolean("ok", false)) {
                        refreshWorkspaceUi()
                        openFileInEditor(rel)
                    } else {
                        Toast.makeText(this, resp.optString("error", "Error"), Toast.LENGTH_LONG).show()
                    }
                }
            }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun refreshConfigUi() {
        val resp = JSONObject(NativeBridge.nativeGetFullStatus())
        if (!resp.optBoolean("ok", false)) return
        val data = resp.optJSONObject("data") ?: return

        val compat = data.optJSONObject("compat")
        if (compat != null) {
            val abi = compat.optString("android_abi")
            val triple = compat.optString("rust_triple")
            val bits = compat.optInt("pointer_bits")
            val sdk = compat.optInt("android_sdk_int")
            val ram = compat.optLong("total_ram_mb")
            val lowRam = compat.optBoolean("low_ram_optimized")
            val activeRt = compat.optJSONObject("active_runtime")
            val rtKind = activeRt?.optString("kind") ?: "compatible_api"
            val rtExplain = if (rtKind == "native_cli") {
                "Native agy CLI (${activeRt?.optString("version")} at ${activeRt?.optString("binary_path")})"
            } else {
                "Pure-Rust API Compatibility (${activeRt?.optString("fallback_reason")})"
            }
            cfgCompatText.text =
                "ABI     : $abi ($triple, ${bits}-bit)\nAndroid : API $sdk | RAM: $ram MB (Low-RAM profile: $lowRam)\nMemory  : ${data.optInt("memory_lines")}/${data.optInt("max_lines")} lines (${data.optInt("memory_bytes")}/${data.optInt("max_bytes")} B)\nActive  : $rtExplain"

            installCliBtn.visibility = if (abi == "arm64-v8a" && sdk >= 24) View.VISIBLE else View.GONE
        }

        val auth = data.optJSONObject("auth")
        if (auth != null) {
            val authed = auth.optBoolean("authenticated")
            val method = auth.optString("method")
            val ks = auth.optBoolean("keystore_protected")
            cfgAuthStatusText.text =
                "Authenticated: ${if (authed) "YES" else "NO"}\nMethod       : $method\nKeystore AES : ${if (ks) "Hardware/Keystore active" else "Sandbox 0600 fallback"}"
        }

        val cfg = data.optJSONObject("config")
        if (cfg != null) {
            val savedCid = cfg.optString("oauth_client_id", "")
            if (savedCid.isNotBlank() && savedCid != "null" && cfgOauthClientIdInput.text.isEmpty()) {
                cfgOauthClientIdInput.setText(savedCid)
            }
            val curModel = cfg.optString("selected_model", "")
            if (curModel.isNotBlank()) {
                cfgCustomModelInput.setText(curModel)
            }
        }

        // Models list
        cfgModelsBox.removeAllViews()
        val models = data.optJSONArray("models")
        if (models != null) {
            for (i in 0 until models.length().coerceAtMost(12)) {
                val m = models.optJSONObject(i) ?: continue
                val id = m.optString("id")
                val name = m.optString("display_name")
                val src = m.optString("source")
                val b = makeButton("$id  [$src]\n$name", primary = false) {
                    NativeBridge.nativeUpdateConfig(id, "", "", "")
                    cfgCustomModelInput.setText(id)
                    refreshTerminalSnapshot(forceRedraw = true)
                    Toast.makeText(this, "Selected model: $id", Toast.LENGTH_SHORT).show()
                }.apply {
                    gravity = Gravity.START or Gravity.CENTER_VERTICAL
                    layoutParams = LinearLayout.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.WRAP_CONTENT
                    ).apply { topMargin = dp(4) }
                }
                cfgModelsBox.addView(b)
            }
        }

        // Sessions list
        cfgSessionsBox.removeAllViews()
        val activeId = data.optJSONObject("active_session")?.optString("id", "") ?: ""
        val sessions = data.optJSONArray("sessions")
        if (sessions == null || sessions.length() == 0) {
            cfgSessionsBox.addView(makeBodyText("No saved sessions yet.", monospace = false))
        } else {
            for (i in 0 until sessions.length().coerceAtMost(15)) {
                val s = sessions.optJSONObject(i) ?: continue
                val sid = s.optString("id")
                val title = s.optString("title")
                val turns = s.optInt("turn_count")
                val isCurrent = (sid == activeId)

                val row = LinearLayout(this).apply {
                    orientation = LinearLayout.HORIZONTAL
                    gravity = Gravity.CENTER_VERTICAL
                    setPadding(0, dp(3), 0, dp(3))
                }
                val resumeBtn = makeButton(
                    "${if (isCurrent) "* " else ""}$title ($turns turns)",
                    primary = isCurrent
                ) {
                    NativeBridge.nativeSessionAction("switch", sid)
                    refreshTerminalSnapshot(forceRedraw = true)
                    switchSection(NavSection.TERMINAL)
                }.apply {
                    gravity = Gravity.START or Gravity.CENTER_VERTICAL
                    layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
                }
                val delBtn = makeButton("Del", primary = false, compactHorizontal = true) {
                    AlertDialog.Builder(this@MainActivity)
                        .setTitle("Delete session")
                        .setMessage("Delete session '$title' ($sid)?")
                        .setPositiveButton("Delete") { _, _ ->
                            NativeBridge.nativeSessionAction("delete", sid)
                            refreshConfigUi()
                            refreshTerminalSnapshot(forceRedraw = true)
                        }
                        .setNegativeButton("Cancel", null)
                        .show()
                }
                row.addView(resumeBtn)
                row.addView(makeHorizontalSpacer(6))
                row.addView(delBtn)
                cfgSessionsBox.addView(row)
            }
        }
        applyPaletteToViews()
    }

    private fun startBrowserOauthFlow() {
        val cid = cfgOauthClientIdInput.text.toString().trim()
        val resp = JSONObject(NativeBridge.nativeStartOAuthPkce(cid))
        if (!resp.optBoolean("ok", false)) {
            Toast.makeText(this, resp.optString("error", "Missing Client ID"), Toast.LENGTH_LONG).show()
            return
        }
        val url = resp.optJSONObject("data")?.optString("authorization_url", "") ?: ""
        if (url.isNotEmpty()) {
            try {
                startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
            } catch (_: Throwable) {
                copyToClipboard("Google OAuth URL", url)
                Toast.makeText(this, "No browser found; copied OAuth URL to clipboard", Toast.LENGTH_LONG).show()
            }
        }
    }

    private fun completeManualOauthCode() {
        val codeOrUrl = cfgOauthCodeInput.text.toString().trim()
        if (codeOrUrl.isEmpty()) {
            Toast.makeText(this, "Paste redirect URL or authorization code first", Toast.LENGTH_SHORT).show()
            return
        }
        bgExecutor.execute {
            val resp = JSONObject(NativeBridge.nativeCompleteOAuthPkce(codeOrUrl))
            uiHandler.post {
                if (resp.optBoolean("ok", false)) {
                    cfgOauthCodeInput.setText("")
                    Toast.makeText(this, "Google OAuth PKCE sign-in complete", Toast.LENGTH_SHORT).show()
                    refreshConfigUi()
                } else {
                    Toast.makeText(this, resp.optString("error", "OAuth exchange failed"), Toast.LENGTH_LONG).show()
                }
            }
        }
    }

    private fun makeCardContainer(): LinearLayout {
        val card = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            val pad = dp(12)
            setPadding(pad, pad, pad, pad)
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT
            ).apply { bottomMargin = dp(12) }
        }
        themedSurfaces.add(card)
        return card
    }

    private fun makeSectionHeading(text: String): TextView {
        val tv = TextView(this).apply {
            typeface = Typeface.create(Typeface.SANS_SERIF, Typeface.BOLD)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            this.text = text
            setPadding(0, 0, 0, dp(4))
        }
        themedPrimaryTexts.add(tv)
        return tv
    }

    private fun makeBodyText(text: String, monospace: Boolean): TextView {
        val tv = TextView(this).apply {
            typeface = if (monospace) Typeface.MONOSPACE else Typeface.SANS_SERIF
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            this.text = text
        }
        themedSecondaryTexts.add(tv)
        return tv
    }

    private fun makeButton(
        label: String,
        primary: Boolean,
        compactHorizontal: Boolean = false,
        onClick: () -> Unit
    ): Button {
        val btn = Button(this).apply {
            text = label
            isAllCaps = false
            typeface = Typeface.create(Typeface.SANS_SERIF, Typeface.BOLD)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            minHeight = dp(48)
            minimumHeight = dp(48)
            minWidth = dp(if (compactHorizontal) 56 else 72)
            minimumWidth = dp(if (compactHorizontal) 56 else 72)
            val hPad = dp(if (compactHorizontal) 10 else 14)
            setPadding(hPad, dp(10), hPad, dp(10))
            isFocusable = true
            setOnClickListener { onClick() }
        }
        themedButtons.add(btn to primary)
        return btn
    }

    private fun makeHairline(heightDp: Int, isAccent: Boolean): View {
        return View(this).apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                dp(heightDp).coerceAtLeast(1)
            )
            setBackgroundColor(if (isAccent) palette.accent else palette.border)
        }
    }

    private fun makeVerticalHairline(): View {
        return View(this).apply {
            layoutParams = LinearLayout.LayoutParams(
                dp(1).coerceAtLeast(1),
                ViewGroup.LayoutParams.MATCH_PARENT
            )
            setBackgroundColor(palette.border)
        }
    }

    private fun makeHorizontalSpacer(widthDp: Int): View {
        return View(this).apply {
            layoutParams = LinearLayout.LayoutParams(dp(widthDp), 1)
        }
    }

    private fun copyToClipboard(label: String, text: String) {
        val cm = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        cm.setPrimaryClip(ClipData.newPlainText(label, text))
    }

    private fun pasteFromClipboard(): String? {
        val cm = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val item = cm.primaryClip?.getItemAt(0) ?: return null
        return item.coerceToText(this)?.toString()
    }

    private fun dp(value: Int): Int = UiMetrics.dp(this, value)

    companion object {
        private const val REQ_OPEN_SAF_TREE = 4101
    }
}
