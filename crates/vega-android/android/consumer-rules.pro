# Applied to whatever consumes this library — the app. Same reason as
# proguard-rules.pro: reflection finds these, a shrinker cannot see that.
-keep class dev.guhhammer.vega.background.** { *; }
