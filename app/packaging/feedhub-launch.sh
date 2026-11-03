#!/bin/sh
# feedhub 的启动器（deb 里装成 /opt/feedhub/bin/feedhub-launch，菜单项的 Exec 指向它）。
#
# 直接用运行时里的 bin/java 起 App，不经过 jpackage 生成的启动器（同目录的 feedhub）：那个启动器每次启动都先跑两遍
# `dpkg -S <自己的路径>` 找自己属于哪个包，这台机器一次 ≈170 ms。
# 参数照 jpackage 的配置 lib/app/feedhub.cfg 拼（类路径、主类、java-options，$APPDIR / $BINDIR / $ROOTDIR 换成实际路径），
# 再加 jpackage 启动器自己会加的 -Djpackage.app-path。cfg 里有不认识的东西（jpackage 升级加了新键）、或者没有 bin/java，
# 就照旧交给 jpackage 的启动器；FEEDHUB_LAUNCHER=jpackage 也强制走它（对照 / 排查用）。
#
# 另外两件事：
# - MALLOC_ARENA_MAX=2：glibc 默认每个核一个 arena，Skia / 解码线程多，空着的 arena 能占几百 MB；
# - AppCDS：第一次运行时把用到的类存成归档（≈45 MB，在 $XDG_CACHE_HOME/feedhub/app-cds.jsa），之后启动直接映射，
#   冷启动快 ≈1/3。版本 / 类路径变了 JVM 自己发现、退出时重建；归档坏了或写不了就当没有，照常启动。
#   直接起 JVM 时放在命令行上；交给 jpackage 启动器时走 JAVA_TOOL_OPTIONS（它不读 JDK_JAVA_OPTIONS）。
# 已经设了的（用户自己 export 的）不覆盖。
[ -n "${MALLOC_ARENA_MAX:-}" ] || MALLOC_ARENA_MAX=2
export MALLOC_ARENA_MAX
# jpackage 启动器不读 JDK_JAVA_OPTIONS，bin/java 会读：照旧不让它影响 App
unset JDK_JAVA_OPTIONS

self=$(readlink -f -- "$0") || self=$0
bin=${self%/*}
root=${bin%/*}
app=$root/lib/app
java=$root/lib/runtime/bin/java
cfg=$app/feedhub.cfg

cache="${XDG_CACHE_HOME:-$HOME/.cache}/feedhub"
cds=
if [ "${FEEDHUB_CDS:-1}" != 0 ] && mkdir -p "$cache" 2>/dev/null && [ -w "$cache" ]; then cds=$cache/app-cds.jsa; fi

# 把 $val 里的 $APPDIR / $BINDIR / $ROOTDIR 换成实际路径（jpackage 启动器认的就这三个）
expand() {
  r=
  while :; do
    case $val in
      *'$'*) ;;
      *) val=$r$val; return ;;
    esac
    r=$r${val%%\$*}
    val=${val#*\$}
    case $val in
      APPDIR*) r=$r$app; val=${val#APPDIR} ;;
      BINDIR*) r=$r$bin; val=${val#BINDIR} ;;
      ROOTDIR*) r=$r$root; val=${val#ROOTDIR} ;;
      *) r=$r\$ ;;
    esac
  done
}

# 读 cfg：类路径用 ':' 连起来，java-options 一行一个（值里不会有换行）
direct=0
if [ "${FEEDHUB_LAUNCHER:-}" != jpackage ] && [ -x "$java" ] && [ -r "$cfg" ]; then
  direct=1 cp= main= opts= section=
  nl='
'
  while IFS= read -r line || [ -n "$line" ]; do
    case $line in
      '') continue ;;
      '['*']') section=$line; continue ;;
      *=*) ;;
      *) direct=0; break ;;
    esac
    key=${line%%=*}
    val=${line#*=}
    expand
    case $section$key in
      '[Application]app.classpath') cp=${cp:+$cp:}$val ;;
      '[Application]app.mainclass') main=$val ;;
      '[JavaOptions]java-options') opts=$opts$val$nl ;;
      *) direct=0; break ;;
    esac
  done < "$cfg"
  [ -n "$cp" ] && [ -n "$main" ] || direct=0
fi

if [ "$direct" = 1 ]; then
  # 命令行：[AppCDS] java-options... -Djpackage.app-path=... -cp <类路径> <主类> <用户参数...>
  # POSIX sh 没有数组：先把 JVM 参数接在用户参数后面，再把用户参数（前 n 个）挨个挪到最后
  n=$#
  [ -z "$cds" ] || set -- "$@" "-XX:SharedArchiveFile=$cds" -XX:+AutoCreateSharedArchive -Xlog:cds=off -Xlog:cds+dynamic=off
  old_ifs=$IFS
  IFS=$nl
  set -f
  # shellcheck disable=SC2086 # 故意按换行切
  set -- "$@" $opts
  set +f
  IFS=$old_ifs
  set -- "$@" "-Djpackage.app-path=$bin/feedhub" -cp "$cp" "$main"
  while [ "$n" -gt 0 ]; do
    set -- "$@" "$1"
    shift
    n=$((n - 1))
  done
  exec "$java" "$@"
fi

# 退路：jpackage 的启动器。JAVA_TOOL_OPTIONS 按空白分隔：路径里有空格就不开 AppCDS
case "$cds" in *[[:space:]]*) cds= ;; esac
if [ -n "$cds" ]; then
  JAVA_TOOL_OPTIONS="-XX:SharedArchiveFile=$cds -XX:+AutoCreateSharedArchive -Xlog:cds=off -Xlog:cds+dynamic=off${JAVA_TOOL_OPTIONS:+ $JAVA_TOOL_OPTIONS}"
  export JAVA_TOOL_OPTIONS
fi
exec "$bin/feedhub" "$@"
