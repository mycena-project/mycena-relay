// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
package io.github.mycena_project.app

import android.content.Intent
import android.os.Bundle
import androidx.appcompat.app.AppCompatActivity
import androidx.activity.compose.setContent
import androidx.compose.material3.Text
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import io.github.mycena_project.app.ui.BulletinScreen
import io.github.mycena_project.app.ui.ChatConversationScreen
import io.github.mycena_project.app.ui.ChatScreen
import io.github.mycena_project.app.ui.MailScreen
import io.github.mycena_project.app.ui.PeersScreen
import io.github.mycena_project.app.ui.SettingsScreen
import io.github.mycena_project.app.ui.WalletScreen

class MainActivity : AppCompatActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        startForegroundService(Intent(this, MeshService::class.java))
        setContent {
            val nav = rememberNavController()
            NavHost(navController = nav, startDestination = "chat") {
                composable("chat") { ChatScreen(nav) }
                composable("chat/{peerId}") { backStack ->
                    ChatConversationScreen(backStack.arguments?.getString("peerId") ?: "")
                }
                composable("bulletin") { BulletinScreen(nav) }
                composable("mail") { MailScreen(nav) }
                composable("peers") { PeersScreen(nav) }
                composable("settings") { SettingsScreen(nav) }
                composable("wallet") { WalletScreen(nav) }
                composable("placeholder") { Text("Mycena") }
            }
        }
    }
}
