package dev.antigravity.mobile.bridge

object NativeBridge {
    init {
        System.loadLibrary("agy_runtime")
    }

    @JvmStatic
    external fun nativeInit(
        filesDir: String,
        sdkInt: Int,
        totalRamMb: Long,
        vaultKey: ByteArray,
        keystoreBacked: Boolean
    ): String

    @JvmStatic
    external fun nativeSubmitInput(input: String): String

    @JvmStatic
    external fun nativeGetTerminalSnapshot(): String

    @JvmStatic
    external fun nativeGetFullStatus(): String

    @JvmStatic
    external fun nativeSendCtrlC(): String

    @JvmStatic
    external fun nativeSendCtrlD(inputEmpty: Boolean): String

    @JvmStatic
    external fun nativeHistoryNav(direction: Int): String

    @JvmStatic
    external fun nativeExportTerminalText(): String

    @JvmStatic
    external fun nativeResolveApproval(approved: Boolean): String

    @JvmStatic
    external fun nativeOnTrimMemory(level: Int)

    @JvmStatic
    external fun nativeSaveApiKey(apiKey: String): String

    @JvmStatic
    external fun nativeStartOAuthPkce(clientId: String): String

    @JvmStatic
    external fun nativeCompleteOAuthPkce(callbackOrCode: String): String

    @JvmStatic
    external fun nativeSignOut(): String

    @JvmStatic
    external fun nativeUpdateConfig(
        model: String,
        mode: String,
        runtimeSelection: String,
        theme: String
    ): String

    @JvmStatic
    external fun nativeRefreshModels(): String

    @JvmStatic
    external fun nativeInstallOfficialCli(): String

    @JvmStatic
    external fun nativeSetWorkspace(rootPath: String, safUri: String): String

    @JvmStatic
    external fun nativeWorkspaceOverview(): String

    @JvmStatic
    external fun nativeWorkspaceReadFile(relPath: String): String

    @JvmStatic
    external fun nativeWorkspaceSaveFile(
        relPath: String,
        content: String,
        confirmed: Boolean
    ): String

    @JvmStatic
    external fun nativeWorkspaceDeleteFile(relPath: String, confirmed: Boolean): String

    @JvmStatic
    external fun nativeWorkspaceSearch(query: String): String

    @JvmStatic
    external fun nativeWorkspaceGitDiff(relPath: String): String

    @JvmStatic
    external fun nativeSessionAction(action: String, arg: String): String
}
