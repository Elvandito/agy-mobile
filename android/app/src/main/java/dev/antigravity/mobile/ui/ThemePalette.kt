package dev.antigravity.mobile.ui

import android.content.Context
import android.content.res.ColorStateList
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.StateListDrawable
import android.util.TypedValue

// Palette definitions matched 1:1 with DESIGN.md and verified >= 4.5:1 (text) and >= 3.0:1 (borders) via contrast-check.py.
data class ThemePalette(
    val isDark: Boolean,
    val canvasBg: Int,
    val surfaceBg: Int,
    val primaryText: Int,
    val secondaryText: Int,
    val accent: Int,
    val accentInk: Int,
    val border: Int,
    val errorText: Int,
    val ansiRed: Int,
    val ansiGreen: Int,
    val ansiAmber: Int,
    val ansiBlue: Int,
    val ansiMagenta: Int,
    val ansiCyan: Int
) {
    companion object {
        val DARK_HARDWARE = ThemePalette(
            isDark = true,
            canvasBg = 0xFF121417.toInt(),
            surfaceBg = 0xFF1B1F24.toInt(),
            primaryText = 0xFFEDEAE2.toInt(),
            secondaryText = 0xFFB2B9C2.toInt(),
            accent = 0xFFF2A93B.toInt(),
            accentInk = 0xFF121417.toInt(),
            border = 0xFF646E7C.toInt(),
            errorText = 0xFFFF8A80.toInt(),
            ansiRed = 0xFFFF8A80.toInt(),
            ansiGreen = 0xFF7EE081.toInt(),
            ansiAmber = 0xFFF2A93B.toInt(),
            ansiBlue = 0xFF82B1FF.toInt(),
            ansiMagenta = 0xFFEA80FC.toInt(),
            ansiCyan = 0xFF64FFDA.toInt()
        )

        val LIGHT_PAPER = ThemePalette(
            isDark = false,
            canvasBg = 0xFFF5F2EB.toInt(),
            surfaceBg = 0xFFEAE5DA.toInt(),
            primaryText = 0xFF16191D.toInt(),
            secondaryText = 0xFF454D57.toInt(),
            accent = 0xFF8A4D00.toInt(),
            accentInk = 0xFFFFFFFF.toInt(),
            border = 0xFF68707B.toInt(),
            errorText = 0xFFA61B1B.toInt(),
            ansiRed = 0xFFA61B1B.toInt(),
            ansiGreen = 0xFF1B5E20.toInt(),
            ansiAmber = 0xFF8A4D00.toInt(),
            ansiBlue = 0xFF0D47A1.toInt(),
            ansiMagenta = 0xFF6A1B9A.toInt(),
            ansiCyan = 0xFF006064.toInt()
        )
    }
}

object UiMetrics {
    fun dp(context: Context, value: Int): Int {
        return TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_DIP,
            value.toFloat(),
            context.resources.displayMetrics
        ).toInt()
    }

    fun borderedSurface(context: Context, palette: ThemePalette, radiusDp: Int = 4): GradientDrawable {
        return GradientDrawable().apply {
            shape = GradientDrawable.RECTANGLE
            cornerRadius = dp(context, radiusDp).toFloat()
            setColor(palette.surfaceBg)
            setStroke(dp(context, 1).coerceAtLeast(1), palette.border)
        }
    }

    fun interactiveButtonBackground(
        context: Context,
        palette: ThemePalette,
        primaryAccent: Boolean
    ): StateListDrawable {
        val radius = dp(context, if (primaryAccent) 6 else 4).toFloat()
        val stroke1 = dp(context, 1).coerceAtLeast(1)
        val stroke2 = dp(context, 2).coerceAtLeast(2)

        val baseFill = if (primaryAccent) palette.accent else palette.surfaceBg
        val pressedFill = if (primaryAccent) {
            if (palette.isDark) 0xFFD99026.toInt() else 0xFF6E3D00.toInt()
        } else {
            if (palette.isDark) 0xFF282E36.toInt() else 0xFFDDD6C7.toInt()
        }
        val disabledFill = if (palette.isDark) 0xFF181B1F.toInt() else 0xFFE2DDD2.toInt()

        fun rect(fill: Int, strokeWidth: Int, strokeColor: Int): GradientDrawable {
            return GradientDrawable().apply {
                shape = GradientDrawable.RECTANGLE
                cornerRadius = radius
                setColor(fill)
                setStroke(strokeWidth, strokeColor)
            }
        }

        return StateListDrawable().apply {
            addState(
                intArrayOf(-android.R.attr.state_enabled),
                rect(disabledFill, stroke1, palette.border)
            )
            addState(
                intArrayOf(android.R.attr.state_pressed),
                rect(pressedFill, stroke2, palette.accent)
            )
            addState(
                intArrayOf(android.R.attr.state_focused),
                rect(baseFill, stroke2, palette.accent)
            )
            addState(
                intArrayOf(),
                rect(baseFill, stroke1, if (primaryAccent) palette.accent else palette.border)
            )
        }
    }

    fun buttonTextColors(palette: ThemePalette, primaryAccent: Boolean): ColorStateList {
        val activeColor = if (primaryAccent) palette.accentInk else palette.primaryText
        return ColorStateList(
            arrayOf(
                intArrayOf(-android.R.attr.state_enabled),
                intArrayOf()
            ),
            intArrayOf(
                palette.secondaryText,
                activeColor
            )
        )
    }
}
