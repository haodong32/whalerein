package com.whalerein.android.state.core

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Base class for the one screen model per screen.
 *
 * The model lives in the JDK-only `:state` module and is the same class the
 * Android composition root hosts, so JVM tests exercise production behavior
 * instead of a parallel implementation.
 */
abstract class ScreenModel<S : Any>(initial: S) : ViewModel() {
    private val mutableState = MutableStateFlow(initial)

    val state: StateFlow<S> = mutableState.asStateFlow()

    protected val scope: CoroutineScope
        get() = viewModelScope

    protected fun update(transform: (S) -> S) {
        mutableState.value = transform(mutableState.value)
    }

    protected fun setState(value: S) {
        mutableState.value = value
    }
}
