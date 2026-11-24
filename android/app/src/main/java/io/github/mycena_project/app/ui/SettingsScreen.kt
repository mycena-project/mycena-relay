// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
package io.github.mycena_project.app.ui

import android.content.Context
import androidx.appcompat.app.AppCompatDelegate
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.ExposedDropdownMenu
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.core.os.LocaleListCompat
import androidx.navigation.NavController
import io.github.mycena_project.app.R
import io.github.mycena_project.bindings.mycena

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(navController: NavController) {
    val name = remember { mutableStateOf(mycena.displayName()) }
    Scaffold(topBar = { TopAppBar(title = { Text(stringResource(R.string.settings_title)) }) }) { padding ->
        Column(modifier = Modifier.fillMaxSize().padding(padding).padding(16.dp)) {
            Text("${stringResource(R.string.settings_display_name)}: ${name.value}")
            Button(
                onClick = {
                    mycena.setDisplayName("AndroidUser")
                    name.value = mycena.displayName()
                },
                modifier = Modifier.padding(top = 12.dp),
            ) {
                Text("Set Demo Name")
            }
            Button(
                onClick = { navController.navigate("wallet") },
                modifier = Modifier.padding(top = 12.dp).fillMaxWidth(),
            ) {
                Text(stringResource(R.string.settings_open_wallet))
            }
            LanguageSelector(modifier = Modifier.padding(top = 24.dp).fillMaxWidth())
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun LanguageSelector(modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val prefs = remember {
        context.getSharedPreferences("settings", Context.MODE_PRIVATE)
    }
    var expanded by remember { mutableStateOf(false) }
    var selected by remember {
        mutableStateOf(prefs.getString("language", "system") ?: "system")
    }
    val languages = remember(context) {
        listOf(
            "system" to context.getString(R.string.language_system),
            "en" to "English",
            "de" to "Deutsch",
            "ru" to "Русский",
            "fa" to "فارسی",
            "zh-CN" to "中文（简体）",
            "ar" to "العربية",
        )
    }
    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { expanded = it },
        modifier = modifier,
    ) {
        OutlinedTextField(
            value = languages.find { it.first == selected }?.second ?: languages.first().second,
            onValueChange = {},
            readOnly = true,
            label = { Text(stringResource(R.string.settings_language)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth(),
        )
        ExposedDropdownMenu(
            expanded = expanded,
            onDismissRequest = { expanded = false },
        ) {
            languages.forEach { (code, label) ->
                DropdownMenuItem(
                    text = { Text(label) },
                    onClick = {
                        selected = code
                        expanded = false
                        prefs.edit().putString("language", code).apply()
                        val locales = when (code) {
                            "system" -> LocaleListCompat.getEmptyLocaleList()
                            "zh-CN" -> LocaleListCompat.forLanguageTags("zh-CN")
                            else -> LocaleListCompat.forLanguageTags(code)
                        }
                        AppCompatDelegate.setApplicationLocales(locales)
                    },
                )
            }
        }
    }
}
