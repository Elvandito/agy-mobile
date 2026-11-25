package dev.antigravity.mobile.ui

import android.content.Context
import android.graphics.Typeface
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.text.style.StyleSpan
import android.text.style.UnderlineSpan
import android.util.TypedValue
import android.view.View
import android.view.ViewGroup
import android.widget.BaseAdapter
import android.widget.TextView
import org.json.JSONArray

data class RenderedTerminalRow(
    val seq: Long,
    val plain: String,
    val spannable: CharSequence
)

class TerminalLineAdapter(
    private val context: Context,
    private var palette: ThemePalette
) : BaseAdapter() {

    private val rows = ArrayList<RenderedTerminalRow>(256)
    private var lastRawJson: JSONArray? = null

    fun updatePalette(nextPalette: ThemePalette) {
        palette = nextPalette
        lastRawJson?.let { rebuildFromJson(it) }
        notifyDataSetChanged()
    }

    fun submitLinesJson(linesArray: JSONArray) {
        lastRawJson = linesArray
        rebuildFromJson(linesArray)
        notifyDataSetChanged()
    }

    private fun rebuildFromJson(linesArray: JSONArray) {
        rows.clear()
        val count = linesArray.length()
        for (i in 0 until count) {
            val lineObj = linesArray.optJSONObject(i) ?: continue
            val seq = lineObj.optLong("seq", i.toLong())
            val spansArr = lineObj.optJSONArray("spans")
            val builder = SpannableStringBuilder()
            val plain = StringBuilder()

            if (spansArr != null) {
                for (j in 0 until spansArr.length()) {
                    val sp = spansArr.optJSONObject(j) ?: continue
                    val txt = sp.optString("text", "")
                    if (txt.isEmpty()) continue
                    val start = builder.length
                    builder.append(txt)
                    plain.append(txt)
                    val end = builder.length

                    val styleObj = sp.optJSONObject("style")
                    if (styleObj != null) {
                        val fgName = styleObj.optString("fg", "default")
                        val fgColor = resolveAnsiColor(fgName)
                        builder.setSpan(
                            ForegroundColorSpan(fgColor),
                            start,
                            end,
                            Spanned.SPAN_EXCLUSIVE_EXCLUSIVE
                        )
                        if (styleObj.optBoolean("bold", false)) {
                            builder.setSpan(
                                StyleSpan(Typeface.BOLD),
                                start,
                                end,
                                Spanned.SPAN_EXCLUSIVE_EXCLUSIVE
                            )
                        }
                        if (styleObj.optBoolean("underline", false)) {
                            builder.setSpan(
                                UnderlineSpan(),
                                start,
                                end,
                                Spanned.SPAN_EXCLUSIVE_EXCLUSIVE
                            )
                        }
                    }
                }
            }
            rows.add(RenderedTerminalRow(seq, plain.toString(), builder))
        }
    }

    private fun resolveAnsiColor(name: String): Int {
        return when (name) {
            "red" -> palette.ansiRed
            "green" -> palette.ansiGreen
            "amber" -> palette.ansiAmber
            "blue" -> palette.ansiBlue
            "magenta" -> palette.ansiMagenta
            "cyan" -> palette.ansiCyan
            "muted" -> palette.secondaryText
            else -> palette.primaryText
        }
    }

    override fun getCount(): Int = rows.size

    override fun getItem(position: Int): RenderedTerminalRow = rows[position]

    override fun getItemId(position: Int): Long = rows[position].seq

    override fun getView(position: Int, convertView: View?, parent: ViewGroup?): View {
        val tv = (convertView as? TextView) ?: TextView(context).apply {
            typeface = Typeface.MONOSPACE
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            val hPad = UiMetrics.dp(context, 10)
            val vPad = UiMetrics.dp(context, 2)
            setPadding(hPad, vPad, hPad, vPad)
            setLineSpacing(UiMetrics.dp(context, 2).toFloat(), 1.0f)
        }
        val item = rows[position]
        tv.setTextColor(palette.primaryText)
        tv.text = item.spannable
        return tv
    }
}
