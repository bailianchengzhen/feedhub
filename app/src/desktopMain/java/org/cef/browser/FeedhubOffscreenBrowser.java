package org.cef.browser;

import org.cef.CefBrowserSettings;
import org.cef.CefClient;
import org.cef.callback.CefDragData;
import org.cef.handler.CefRenderHandler;
import org.cef.handler.CefScreenInfo;

import java.awt.Component;
import java.awt.Point;
import java.awt.Rectangle;
import java.awt.event.KeyEvent;
import java.awt.event.MouseEvent;
import java.awt.event.MouseWheelEvent;
import java.awt.image.BufferedImage;
import java.nio.ByteBuffer;
import java.util.concurrent.CompletableFuture;
import java.util.function.Consumer;

/**
 * 只在**验证码子进程**里用的 OSR 浏览器（{@code feedhub.app.captcha.helper}）：和 JCEF 自带的 {@code CefBrowserOsr} 一样走离屏渲染，
 * 但**不建 OpenGL 画布、不挂任何 AWT 窗口**——CEF 的 {@code onPaint} 直接交给 {@link Sink}，由子进程把像素传回 App。
 *
 * <p>放在 {@code org.cef.browser} 包里、用 Java 写，是因为基类 {@code CefBrowser_N} 和发输入事件的方法（{@code sendMouseEvent} 等）
 * 是包内可见的（Kotlin 不允许 public 类继承包内可见的 Java 类）；JCEF 的 jar 没有 seal 包，类路径方式运行可以这样扩展。
 * 版本钉在 JCEF 135.0.20（见 {@code libs.versions.toml}），升级时要对一下 {@code CefBrowserOsr} 的写法有没有变。
 *
 * <p>尺寸用 CSS 像素（视图宽高），{@code scale} 是设备像素比：CEF 出的帧是"视图 × scale"的设备像素，
 * 鼠标坐标是 CSS 像素——和 App 那边 Compose 的 {@code px / density} 对上，滑块不会错位。
 */
public final class FeedhubOffscreenBrowser extends CefBrowser_N implements CefRenderHandler {
    /** 子进程接帧和光标的地方。 */
    public interface Sink {
        void onPaint(boolean popup, Rectangle[] dirty, ByteBuffer buffer, int width, int height);

        void onCursor(int awtCursorType);
    }

    private final Sink sink;
    private volatile int viewWidth = 1;
    private volatile int viewHeight = 1;
    private volatile double scale = 1.0;

    /** AWT 事件要一个来源组件；不显示、不加到任何容器里（headless 也能建）。 */
    private final Component source = new Component() {};

    public FeedhubOffscreenBrowser(CefClient client, String url, CefBrowserSettings settings, Sink sink) {
        super(client, url, null, null, null, settings);
        this.sink = sink;
    }

    /** 设视图大小和设备像素比；浏览器建好之后调用（{@code notify}）会通知 CEF 重新取。 */
    public void setView(int width, int height, double deviceScale, boolean notify) {
        viewWidth = Math.max(1, width);
        viewHeight = Math.max(1, height);
        scale = deviceScale;
        if (notify) wasResized(viewWidth, viewHeight);
    }

    @Override
    public void createImmediately() {
        createBrowser(getClient(), 0, getUrl(), true, false, null, getRequestContext());
    }

    public void mouse(int id, int x, int y, int modifiers, int button, int clickCount) {
        sendMouseEvent(new MouseEvent(source, id, System.currentTimeMillis(), modifiers, x, y, x, y, clickCount, false, button));
    }

    public void wheel(int x, int y, int modifiers, int rotation, double precise) {
        sendMouseWheelEvent(new MouseWheelEvent(source, MouseEvent.MOUSE_WHEEL, System.currentTimeMillis(), modifiers, x, y, x, y, 0, false,
                MouseWheelEvent.WHEEL_UNIT_SCROLL, 3, rotation, precise));
    }

    public void key(int id, int keyCode, char keyChar, int modifiers, int location) {
        sendKeyEvent(new KeyEvent(source, id, System.currentTimeMillis(), modifiers, keyCode, keyChar, location));
    }

    // —— CefBrowser：没有 UI 组件 ——
    @Override
    public Component getUIComponent() {
        return source;
    }

    @Override
    public CefRenderHandler getRenderHandler() {
        return this;
    }

    @Override
    protected CefBrowser_N createDevToolsBrowser(CefClient client, String url, CefRequestContext context, CefBrowser_N parent, Point inspectAt) {
        throw new UnsupportedOperationException("验证码浏览器不开开发者工具");
    }

    @Override
    public CompletableFuture<BufferedImage> createScreenshot(boolean nativeResolution) {
        return CompletableFuture.failedFuture(new UnsupportedOperationException());
    }

    // —— CefRenderHandler ——
    @Override
    public Rectangle getViewRect(CefBrowser browser) {
        return new Rectangle(0, 0, viewWidth, viewHeight);
    }

    @Override
    public boolean getScreenInfo(CefBrowser browser, CefScreenInfo screenInfo) {
        Rectangle r = new Rectangle(0, 0, viewWidth, viewHeight);
        screenInfo.Set(scale, 32, 8, false, r, r);
        return true;
    }

    @Override
    public Point getScreenPoint(CefBrowser browser, Point viewPoint) {
        return new Point(viewPoint);
    }

    @Override
    public void onPopupShow(CefBrowser browser, boolean show) {}

    @Override
    public void onPopupSize(CefBrowser browser, Rectangle size) {}

    @Override
    public void onPaint(CefBrowser browser, boolean popup, Rectangle[] dirtyRects, ByteBuffer buffer, int width, int height) {
        sink.onPaint(popup, dirtyRects, buffer, width, height);
    }

    @Override
    public void addOnPaintListener(Consumer<CefPaintEvent> listener) {}

    @Override
    public void setOnPaintListener(Consumer<CefPaintEvent> listener) {}

    @Override
    public void removeOnPaintListener(Consumer<CefPaintEvent> listener) {}

    @Override
    public boolean onCursorChange(CefBrowser browser, int cursorType) {
        sink.onCursor(cursorType);
        return true;
    }

    /** 不做拖放（HTML5 DnD）：返回 false 让 CEF 取消这次拖放，滑块用的是普通鼠标事件，不受影响。 */
    @Override
    public boolean startDragging(CefBrowser browser, CefDragData dragData, int mask, int x, int y) {
        return false;
    }

    @Override
    public void updateDragCursor(CefBrowser browser, int operation) {}
}
