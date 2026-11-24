// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
package io.github.mycena_project.app.viewmodel

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.stateIn
import io.github.mycena_project.bindings.BulletinPost
import io.github.mycena_project.bindings.mycena

class BulletinViewModel(application: Application) : AndroidViewModel(application) {
    val posts: StateFlow<List<BulletinPost>> =
        flow {
            while (true) {
                emit(mycena.bulletinsForScope("mycena/global"))
                delay(3_000)
            }
        }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())
}
