// The output of `kargo target list --format json`, read without the IDE, so the parsing can be unit tested.

package io.github.enekocm7.kargo.actions

import com.google.gson.JsonParser

/** The names of the targets `enabled` lists in the output of `kargo target list --format json`. */
fun enabledTargetNames(json: String): List<String> =
    JsonParser.parseString(json).asJsonObject.getAsJsonArray("enabled").map { it.asJsonObject.get("name").asString }
