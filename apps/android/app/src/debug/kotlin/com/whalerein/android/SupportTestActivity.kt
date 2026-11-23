package com.whalerein.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import com.whalerein.android.platform.documents.AndroidDocumentGateway
import com.whalerein.android.platform.lan.ActivityLanPermission
import com.whalerein.android.state.core.DiagnosticEvent
import com.whalerein.android.state.core.DiagnosticSink

/**
 * Debug-only Activity host for instrumentation tests.
 *
 * ActivityResult launchers must be registered before the activity starts, so
 * the gateways are constructed in `onCreate` rather than by a test after resume.
 */
class SupportTestActivity : ComponentActivity() {
    lateinit var lanPermission: ActivityLanPermission
        private set
    lateinit var documents: AndroidDocumentGateway
        private set

    private val testSink = object : DiagnosticSink {
        override fun emit(event: DiagnosticEvent) = Unit
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        lanPermission = ActivityLanPermission(this, testSink)
        documents = AndroidDocumentGateway(this)
        super.onCreate(savedInstanceState)
    }
}
