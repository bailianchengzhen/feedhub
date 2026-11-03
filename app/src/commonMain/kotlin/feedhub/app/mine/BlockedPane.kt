package feedhub.app.mine

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import feedhub.app.feed.Avatar
import feedhub.app.feed.PlatformTag
import feedhub.app.library.Library
import feedhub.app.library.blockKey

/** 屏蔽的用户：列表 + 解除屏蔽（屏蔽在详情页顶栏做）。 */
@Composable
fun BlockedPane(library: Library, onBack: (() -> Unit)?) {
    val users = library.blockedUsers
    PaneScaffold("屏蔽的用户", onBack, subtitle = if (users.isEmpty()) null else "共 ${users.size} 人") {
        if (users.isEmpty()) {
            EmptyHint("没有屏蔽任何人。在详情页顶栏点\"屏蔽\"，这个作者的内容就不再出现在首页和关注里。")
            return@PaneScaffold
        }
        LazyColumn(
            contentPadding = PaddingValues(start = 12.dp, end = 12.dp, top = 4.dp, bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.fillMaxSize(),
        ) {
            items(users, key = { it.blockKey() }) { u ->
                Surface(shape = RoundedCornerShape(12.dp), color = MaterialTheme.colorScheme.surfaceContainerLowest, modifier = Modifier.fillMaxWidth()) {
                    Row(Modifier.padding(horizontal = 12.dp, vertical = 10.dp), verticalAlignment = Alignment.CenterVertically) {
                        Avatar(u.avatar, 40)
                        Spacer(Modifier.width(12.dp))
                        Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            OneLine(u.name, Modifier.weight(1f, fill = false))
                            PlatformTag(u.platform)
                        }
                        Spacer(Modifier.width(8.dp))
                        OutlinedButton(onClick = { library.unblock(u) }) { Text("解除屏蔽") }
                    }
                }
            }
        }
    }
}
