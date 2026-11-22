package com.whalerein.android.ui.access

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.whalerein.android.R

/** Debug-only Cloud environment switch shown on the sign-in screen. */
@Composable
fun DebugEnvironmentPicker(
    current: String,
    hasCa: Boolean,
    busy: Boolean,
    error: String?,
    onSelect: (String) -> Unit,
    onChooseCa: () -> Unit,
    onApply: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val choices = listOf(
        "production" to R.string.access_debug_production,
        "staging" to R.string.access_debug_staging,
        "local" to R.string.access_debug_local,
    )
    Card(modifier = modifier.fillMaxWidth().padding(16.dp)) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                text = stringResource(R.string.access_debug_environment),
                style = MaterialTheme.typography.titleSmall,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                for ((value, label) in choices) {
                    FilterChip(
                        selected = current == value,
                        onClick = { onSelect(value) },
                        enabled = !busy,
                        label = { Text(stringResource(label)) },
                    )
                }
            }
            if (current == "local") {
                Text(
                    text = stringResource(
                        if (hasCa) R.string.access_debug_ca_hint else R.string.access_debug_ca_required,
                    ),
                    style = MaterialTheme.typography.bodySmall,
                )
                OutlinedButton(
                    onClick = onChooseCa,
                    enabled = !busy,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(stringResource(R.string.access_debug_choose_ca))
                }
            }
            error?.let {
                Text(
                    text = it,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
            Button(
                onClick = onApply,
                enabled = !busy,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(stringResource(R.string.access_debug_apply))
            }
        }
    }
}
