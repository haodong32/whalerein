package com.whalerein.android.state.testing

import org.junit.jupiter.api.extension.ConditionEvaluationResult
import org.junit.jupiter.api.extension.ExecutionCondition
import org.junit.jupiter.api.extension.ExtensionContext

/**
 * Selects one acceptance case before its test body executes.
 *
 * The repository runner passes `-PcaseFilter=<matrix case id>`; this condition
 * disables every other test so a focused run never executes unrelated bodies.
 * `-PcaseList=true` disables all bodies so the runner can discover case IDs
 * from the emitted XML without executing them.
 */
class CaseFilterExtension : ExecutionCondition {
    override fun evaluateExecutionCondition(context: ExtensionContext): ConditionEvaluationResult {
        if (context.testMethod.isEmpty) {
            return ConditionEvaluationResult.enabled("container")
        }
        if (System.getProperty(LIST_PROPERTY).toBoolean()) {
            return ConditionEvaluationResult.disabled("case listing")
        }
        val filter = System.getProperty(FILTER_PROPERTY).orEmpty()
        if (filter.isBlank()) {
            return ConditionEvaluationResult.enabled("no case filter")
        }
        val haystack = buildString {
            append(context.displayName)
            append(' ')
            append(context.testMethod.get().name)
            append(' ')
            context.testClass.ifPresent {
                append(it.name)
                append(' ')
                append(it.simpleName)
                append(' ')
                append(it.packageName)
            }
            append(' ')
            append(context.uniqueId)
        }
        return if (haystack.contains(filter)) {
            ConditionEvaluationResult.enabled("matches $filter")
        } else {
            ConditionEvaluationResult.disabled("does not match $filter")
        }
    }

    private companion object {
        const val FILTER_PROPERTY = "whalerein.caseFilter"
        const val LIST_PROPERTY = "whalerein.caseList"
    }
}
