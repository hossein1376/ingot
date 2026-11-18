; Ingot back end
target triple = "arm64-apple-macosx14.0.0"

%t0 = type { i64, i64, [0 x i64] }
%t1 = type [2 x i64]
%t2 = type { ptr, i64 }
%t3 = type { i8, [7 x i8], [0 x i64], [8 x i8] }
%t4 = type [1 x i8]
%t5 = type { ptr, i64 }
%t6 = type { i8, [0 x i8] }
%t7 = type { ptr, i64 }
%t8 = type {}
%t9 = type {}
%t10 = type {}
%t11 = type {}
%t12 = type {}
%t13 = type {}
%t14 = type {}
%t15 = type {}
%t16 = type {}
%t17 = type { i8, [0 x i8] }
%t18 = type { %t7, ptr, %t9, %t10, ptr, ptr, ptr, ptr, ptr, ptr, ptr, %t17, [7 x i8], [0 x i64] }
%t19 = type { %t6, [7 x i8], %t5, i32, i32, %t5, [0 x i64] }

@str.0 = private unnamed_addr constant [11 x i8] c"small.ingot"
@str.1 = private unnamed_addr constant [21 x i8] c"integer overflow in +"
@str.2 = private unnamed_addr constant [32 x i8] c"assertion failed: p.a + p.b == 3"
@str.3 = private unnamed_addr constant [30 x i8] c"assertion failed: p.sum() == 3"
@str.4 = private unnamed_addr constant [40 x i8] c"assertion failed: pick(&a[0..2], 1) == 4"
@str.5 = private unnamed_addr constant [27 x i8] c"assertion failed: z[0] == 0"
@str.6 = private unnamed_addr constant [24 x i8] c"assertion failed: v == 3"
@str.7 = private unnamed_addr constant [23 x i8] c"assertion failed: false"
@str.8 = private unnamed_addr constant [34 x i8] c"index \01 out of bounds for length \02"
@str.9 = private unnamed_addr constant [7 x i8] c": trap "
@str.10 = private unnamed_addr constant [2 x i8] c": "
@str.11 = private unnamed_addr constant [16 x i8] c"std/sys/rt.ingot"
@str.12 = private unnamed_addr constant [24 x i8] c"entered unreachable code"
@str.13 = private unnamed_addr constant [3 x i8] c"T01"
@str.14 = private unnamed_addr constant [3 x i8] c"T02"
@str.15 = private unnamed_addr constant [3 x i8] c"T03"
@str.16 = private unnamed_addr constant [3 x i8] c"T04"
@str.17 = private unnamed_addr constant [3 x i8] c"T05"
@str.18 = private unnamed_addr constant [3 x i8] c"T06"
@str.19 = private unnamed_addr constant [3 x i8] c"T07"
@str.20 = private unnamed_addr constant [3 x i8] c"T08"
@str.21 = private unnamed_addr constant [3 x i8] c"T09"
@str.22 = private unnamed_addr constant [3 x i8] c"T10"
@str.23 = private unnamed_addr constant [3 x i8] c"T11"
@str.24 = private unnamed_addr constant [3 x i8] c"T12"
@str.25 = private unnamed_addr constant [3 x i8] c"T13"
@str.26 = private unnamed_addr constant [3 x i8] c"T14"
@str.27 = private unnamed_addr constant [3 x i8] c"T15"
@str.28 = private unnamed_addr constant [3 x i8] c"T16"
@str.29 = private unnamed_addr constant [3 x i8] c"T17"
@str.30 = private unnamed_addr constant [21 x i8] c"integer overflow in *"
@str.31 = private unnamed_addr constant [9 x i8] c"<runtime>"
@str.32 = private unnamed_addr constant [20 x i8] c"no memory for a task"
@site.0 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 2, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.0, i64 11 }, i32 10, i32 16, { ptr, i64 } { ptr @str.1, i64 21 } }
@site.1 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 2, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.0, i64 11 }, i32 31, i32 13, { ptr, i64 } { ptr @str.1, i64 21 } }
@site.2 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 7, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.0, i64 11 }, i32 31, i32 5, { ptr, i64 } { ptr @str.2, i64 32 } }
@site.3 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 7, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.0, i64 11 }, i32 32, i32 5, { ptr, i64 } { ptr @str.3, i64 30 } }
@site.4 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 7, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.0, i64 11 }, i32 34, i32 5, { ptr, i64 } { ptr @str.4, i64 40 } }
@site.5 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 7, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.0, i64 11 }, i32 36, i32 5, { ptr, i64 } { ptr @str.5, i64 27 } }
@site.6 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 7, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.0, i64 11 }, i32 40, i32 13, { ptr, i64 } { ptr @str.6, i64 24 } }
@site.7 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 7, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.0, i64 11 }, i32 43, i32 13, { ptr, i64 } { ptr @str.7, i64 23 } }
@site.8 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 0, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.0, i64 11 }, i32 19, i32 12, { ptr, i64 } { ptr @str.8, i64 34 } }
@site.9 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 9, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.11, i64 16 }, i32 201, i32 5, { ptr, i64 } { ptr @str.12, i64 24 } }
@site.10 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 2, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.11, i64 16 }, i32 198, i32 13, { ptr, i64 } { ptr @str.1, i64 21 } }
@site.11 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 2, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.11, i64 16 }, i32 78, i32 10, { ptr, i64 } { ptr @str.1, i64 21 } }
@site.12 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 2, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.11, i64 16 }, i32 222, i32 21, { ptr, i64 } { ptr @str.30, i64 21 } }
@site.13 = private constant { i8, [7 x i8], { ptr, i64 }, i32, i32, { ptr, i64 } } { i8 2, [7 x i8] zeroinitializer, { ptr, i64 } { ptr @str.11, i64 16 }, i32 245, i32 13, { ptr, i64 } { ptr @str.1, i64 21 } }

declare void @exit(i32) noreturn nounwind
declare { i64, i1 } @llvm.sadd.with.overflow.i64(i64, i64) nounwind readnone
declare { i64, i1 } @llvm.smul.with.overflow.i64(i64, i64) nounwind readnone
declare { i8, i1 } @llvm.uadd.with.overflow.i8(i8, i8) nounwind readnone
declare ptr @mmap(ptr, i64, i32, i32, i32, i64) nounwind
declare i64 @write(i32, ptr, i64) nounwind

define internal i64 @"_I1:t1:t12:t.Pair[].sum8:t.Pair[]"(ptr %arg0) nounwind #0 {
bb0:
  %l0 = alloca ptr, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca i64, align 8
  %l3 = alloca i64, align 8
  %l4 = alloca i64, align 8
  store ptr %arg0, ptr %l0
  %tmp0 = load ptr, ptr %l0
  %tmp1 = getelementptr i8, ptr %tmp0, i64 0
  %tmp2 = load i64, ptr %tmp1
  store i64 %tmp2, ptr %l1
  %tmp3 = load ptr, ptr %l0
  %tmp4 = getelementptr i8, ptr %tmp3, i64 8
  %tmp5 = load i64, ptr %tmp4
  store i64 %tmp5, ptr %l2
  %tmp6 = load i64, ptr %l1
  %tmp7 = load i64, ptr %l2
  %tmp8 = add i64 %tmp6, %tmp7
  store i64 %tmp8, ptr %l3
  %tmp9 = load i64, ptr %l1
  %tmp10 = load i64, ptr %l2
  %tmp11 = call { i64, i1 } @llvm.sadd.with.overflow.i64(i64 %tmp9, i64 %tmp10)
  %tmp12 = extractvalue { i64, i1 } %tmp11, 1
  br i1 %tmp12, label %trap0, label %cont1
cont1:
  %tmp13 = load i64, ptr %l3
  store i64 %tmp13, ptr %l4
  %tmp14 = load i64, ptr %l4
  ret i64 %tmp14
trap0:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.0, i64 0, i64 0, i64 0)
  unreachable
}

define internal i8 @"_I1:t1:t6:t.main"(ptr %arg0) nounwind #0 {
bb0:
  %tmp0 = alloca %t0, align 8
  %tmp28 = alloca %t1, align 8
  %tmp56 = alloca [8 x i8], align 8
  %l0 = alloca ptr, align 8
  %l1 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l1
  %l2 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l2
  %l3 = alloca i64, align 8
  %l4 = alloca i64, align 8
  %l5 = alloca i64, align 8
  %l6 = alloca i64, align 8
  %l7 = alloca i1, align 1
  %l8 = alloca i1, align 1
  %l9 = alloca ptr, align 8
  %l10 = alloca i64, align 8
  %l11 = alloca i64, align 8
  %l12 = alloca i1, align 1
  %l13 = alloca i1, align 1
  %l14 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l14
  %l15 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l15
  %l16 = alloca %t2, align 8
  %l17 = alloca i64, align 8
  %l18 = alloca i64, align 8
  %l19 = alloca i1, align 1
  %l20 = alloca i1, align 1
  %l21 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l21
  %l22 = alloca i64, align 8
  %l23 = alloca i1, align 1
  %l24 = alloca i1, align 1
  %l25 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l25
  %l26 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l26
  %l27 = alloca i8, align 1
  %l28 = alloca i1, align 1
  %l29 = alloca i64, align 8
  %l30 = alloca i64, align 8
  %l31 = alloca i1, align 1
  %l32 = alloca i1, align 1
  %l33 = alloca i8, align 1
  %l34 = alloca i1, align 1
  store ptr %arg0, ptr %l0
  store %t0 zeroinitializer, ptr %tmp0
  %tmp1 = getelementptr i8, ptr %tmp0, i64 0
  store i64 1, ptr %tmp1
  %tmp2 = getelementptr i8, ptr %tmp0, i64 8
  store i64 2, ptr %tmp2
  %tmp3 = load %t0, ptr %tmp0
  store %t0 %tmp3, ptr %l1
  %tmp4 = load %t0, ptr %l1
  store %t0 %tmp4, ptr %l2
  %tmp5 = getelementptr i8, ptr %l2, i64 0
  %tmp6 = load i64, ptr %tmp5
  store i64 %tmp6, ptr %l3
  %tmp7 = getelementptr i8, ptr %l2, i64 8
  %tmp8 = load i64, ptr %tmp7
  store i64 %tmp8, ptr %l4
  %tmp9 = load i64, ptr %l3
  %tmp10 = load i64, ptr %l4
  %tmp11 = add i64 %tmp9, %tmp10
  store i64 %tmp11, ptr %l5
  %tmp12 = load i64, ptr %l3
  %tmp13 = load i64, ptr %l4
  %tmp14 = call { i64, i1 } @llvm.sadd.with.overflow.i64(i64 %tmp12, i64 %tmp13)
  %tmp15 = extractvalue { i64, i1 } %tmp14, 1
  br i1 %tmp15, label %trap0, label %cont1
bb1:
  store ptr %l2, ptr %l9
  %tmp21 = load ptr, ptr %l9
  %tmp22 = call i64 @"_I1:t1:t12:t.Pair[].sum8:t.Pair[]"(ptr %tmp21)
  store i64 %tmp22, ptr %l10
  br label %bb3
bb2:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.2, i64 0, i64 0, i64 0)
  unreachable
bb3:
  %tmp23 = load i64, ptr %l10
  store i64 %tmp23, ptr %l11
  %tmp24 = load i64, ptr %l11
  %tmp25 = icmp eq i64 %tmp24, 3
  store i1 %tmp25, ptr %l12
  %tmp26 = load i1, ptr %l12
  store i1 %tmp26, ptr %l13
  %tmp27 = load i1, ptr %l13
  br i1 %tmp27, label %bb4, label %bb5
bb4:
  store %t1 zeroinitializer, ptr %tmp28
  %tmp29 = getelementptr %t1, ptr %tmp28, i64 0, i64 0
  store i64 3, ptr %tmp29
  %tmp30 = getelementptr %t1, ptr %tmp28, i64 0, i64 1
  store i64 4, ptr %tmp30
  %tmp31 = load %t1, ptr %tmp28
  store %t1 %tmp31, ptr %l14
  %tmp32 = load %t1, ptr %l14
  store %t1 %tmp32, ptr %l15
  %tmp33 = getelementptr %t1, ptr %l15, i64 0, i64 0
  %tmp34 = getelementptr i64, ptr %tmp33, i64 0
  %tmp35 = sub i64 2, 0
  %tmp36 = insertvalue %t2 zeroinitializer, ptr %tmp34, 0
  %tmp37 = insertvalue %t2 %tmp36, i64 %tmp35, 1
  store %t2 %tmp37, ptr %l16
  %tmp38 = load %t2, ptr %l16
  %tmp39 = extractvalue %t2 %tmp38, 0
  %tmp40 = extractvalue %t2 %tmp38, 1
  %tmp41 = call i64 @"_I1:t1:t6:t.pick"(ptr %tmp39, i64 %tmp40, i64 1)
  store i64 %tmp41, ptr %l17
  br label %bb6
bb5:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.3, i64 0, i64 0, i64 0)
  unreachable
bb6:
  %tmp42 = load i64, ptr %l17
  store i64 %tmp42, ptr %l18
  %tmp43 = load i64, ptr %l18
  %tmp44 = icmp eq i64 %tmp43, 4
  store i1 %tmp44, ptr %l19
  %tmp45 = load i1, ptr %l19
  store i1 %tmp45, ptr %l20
  %tmp46 = load i1, ptr %l20
  br i1 %tmp46, label %bb7, label %bb8
bb7:
  store %t1 zeroinitializer, ptr %l21
  %tmp47 = getelementptr %t1, ptr %l21, i64 0, i64 0
  %tmp48 = load i64, ptr %tmp47
  store i64 %tmp48, ptr %l22
  %tmp49 = load i64, ptr %l22
  %tmp50 = icmp eq i64 %tmp49, 0
  store i1 %tmp50, ptr %l23
  %tmp51 = load i1, ptr %l23
  store i1 %tmp51, ptr %l24
  %tmp52 = load i1, ptr %l24
  br i1 %tmp52, label %bb9, label %bb10
bb8:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.4, i64 0, i64 0, i64 0)
  unreachable
bb9:
  %tmp53 = call { i8, i64 } @"_I1:t1:t7:t.maybe"(i64 3)
  %tmp54 = extractvalue { i8, i64 } %tmp53, 0
  store i8 %tmp54, ptr %l25
  %tmp55 = extractvalue { i8, i64 } %tmp53, 1
  store [8 x i8] zeroinitializer, ptr %tmp56
  store i64 %tmp55, ptr %tmp56
  %tmp57 = getelementptr i8, ptr %l25, i64 8
  %tmp58 = load i64, ptr %tmp56, align 1
  store i64 %tmp58, ptr %tmp57, align 1
  br label %bb11
bb10:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.5, i64 0, i64 0, i64 0)
  unreachable
bb11:
  %tmp59 = load %t3, ptr %l25
  store %t3 %tmp59, ptr %l26
  %tmp60 = load i8, ptr %l26
  store i8 %tmp60, ptr %l27
  %tmp61 = load i8, ptr %l27
  %tmp62 = icmp eq i8 %tmp61, 1
  store i1 %tmp62, ptr %l28
  %tmp63 = load i1, ptr %l28
  br i1 %tmp63, label %bb14, label %bb15
bb12:
  ret i8 0
bb13:
  %tmp64 = load i8, ptr %l26
  store i8 %tmp64, ptr %l33
  %tmp65 = load i8, ptr %l33
  %tmp66 = icmp eq i8 %tmp65, 0
  store i1 %tmp66, ptr %l34
  %tmp67 = load i1, ptr %l34
  br i1 %tmp67, label %bb19, label %bb20
bb14:
  %tmp68 = getelementptr i8, ptr %l26, i64 8
  %tmp69 = load i64, ptr %tmp68
  store i64 %tmp69, ptr %l29
  %tmp70 = load i64, ptr %l29
  store i64 %tmp70, ptr %l30
  %tmp71 = load i64, ptr %l30
  %tmp72 = icmp eq i64 %tmp71, 3
  store i1 %tmp72, ptr %l31
  %tmp73 = load i1, ptr %l31
  store i1 %tmp73, ptr %l32
  %tmp74 = load i1, ptr %l32
  br i1 %tmp74, label %bb16, label %bb17
bb15:
  br label %bb13
bb16:
  br label %bb12
bb17:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.6, i64 0, i64 0, i64 0)
  unreachable
bb18:
  unreachable
bb19:
  br i1 false, label %bb21, label %bb22
bb20:
  br label %bb18
bb21:
  br label %bb12
bb22:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.7, i64 0, i64 0, i64 0)
  unreachable
cont1:
  %tmp16 = load i64, ptr %l5
  store i64 %tmp16, ptr %l6
  %tmp17 = load i64, ptr %l6
  %tmp18 = icmp eq i64 %tmp17, 3
  store i1 %tmp18, ptr %l7
  %tmp19 = load i1, ptr %l7
  store i1 %tmp19, ptr %l8
  %tmp20 = load i1, ptr %l8
  br i1 %tmp20, label %bb1, label %bb2
trap0:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.1, i64 0, i64 0, i64 0)
  unreachable
}

define internal i64 @"_I1:t1:t6:t.pick"(ptr %arg0, i64 %arg1, i64 %arg2) nounwind #0 {
bb0:
  %l0 = alloca %t2, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca i64, align 8
  %l3 = alloca i64, align 8
  %tmp0 = insertvalue %t2 zeroinitializer, ptr %arg0, 0
  %tmp1 = insertvalue %t2 %tmp0, i64 %arg1, 1
  store %t2 %tmp1, ptr %l0
  store i64 %arg2, ptr %l1
  %tmp2 = load i64, ptr %l1
  store i64 %tmp2, ptr %l2
  %tmp3 = getelementptr i8, ptr %l0, i64 8
  %tmp4 = load i64, ptr %tmp3
  %tmp5 = load i64, ptr %l2
  %tmp6 = icmp slt i64 %tmp5, 0
  %tmp7 = icmp uge i64 %tmp5, %tmp4
  %tmp8 = or i1 %tmp6, %tmp7
  br i1 %tmp8, label %trap0, label %cont1
cont1:
  %tmp9 = load i64, ptr %l2
  %tmp11 = load ptr, ptr %l0
  %tmp10 = getelementptr i64, ptr %tmp11, i64 %tmp9
  %tmp12 = load i64, ptr %tmp10
  store i64 %tmp12, ptr %l3
  %tmp13 = load i64, ptr %l3
  ret i64 %tmp13
trap0:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.8, i64 %tmp5, i64 %tmp4, i64 0)
  unreachable
}

define internal { i8, i64 } @"_I1:t1:t7:t.maybe"(i64 %arg0) nounwind #0 {
bb0:
  %tmp6 = alloca %t3, align 8
  %tmp12 = alloca %t3, align 8
  %tmp14 = alloca [8 x i8], align 8
  %tmp20 = alloca %t3, align 8
  %tmp24 = alloca %t3, align 8
  %tmp26 = alloca [8 x i8], align 8
  %l0 = alloca i64, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca i1, align 1
  %l3 = alloca i1, align 1
  %l4 = alloca i64, align 8
  %l5 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l5
  %l6 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l6
  %l7 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l7
  %l8 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l8
  store i64 %arg0, ptr %l0
  %tmp0 = load i64, ptr %l0
  store i64 %tmp0, ptr %l1
  %tmp1 = load i64, ptr %l1
  %tmp2 = icmp sgt i64 %tmp1, 0
  store i1 %tmp2, ptr %l2
  %tmp3 = load i1, ptr %l2
  store i1 %tmp3, ptr %l3
  %tmp4 = load i1, ptr %l3
  br i1 %tmp4, label %bb1, label %bb2
bb1:
  %tmp5 = load i64, ptr %l0
  store i64 %tmp5, ptr %l4
  store %t3 zeroinitializer, ptr %tmp6
  store i8 1, ptr %tmp6
  %tmp7 = load i64, ptr %l4
  %tmp8 = getelementptr i8, ptr %tmp6, i64 8
  store i64 %tmp7, ptr %tmp8
  %tmp9 = load %t3, ptr %tmp6
  store %t3 %tmp9, ptr %l5
  %tmp10 = load %t3, ptr %l5
  store %t3 %tmp10, ptr %l6
  %tmp11 = load %t3, ptr %l6
  store %t3 %tmp11, ptr %tmp12
  %tmp13 = load i8, ptr %tmp12
  store [8 x i8] zeroinitializer, ptr %tmp14
  %tmp15 = getelementptr i8, ptr %tmp12, i64 8
  %tmp16 = load i64, ptr %tmp15, align 1
  store i64 %tmp16, ptr %tmp14, align 1
  %tmp17 = load i64, ptr %tmp14
  %tmp18 = insertvalue { i8, i64 } zeroinitializer, i8 %tmp13, 0
  %tmp19 = insertvalue { i8, i64 } %tmp18, i64 %tmp17, 1
  ret { i8, i64 } %tmp19
bb2:
  br label %bb3
bb3:
  store %t3 zeroinitializer, ptr %tmp20
  store i8 0, ptr %tmp20
  %tmp21 = load %t3, ptr %tmp20
  store %t3 %tmp21, ptr %l7
  %tmp22 = load %t3, ptr %l7
  store %t3 %tmp22, ptr %l8
  %tmp23 = load %t3, ptr %l8
  store %t3 %tmp23, ptr %tmp24
  %tmp25 = load i8, ptr %tmp24
  store [8 x i8] zeroinitializer, ptr %tmp26
  %tmp27 = getelementptr i8, ptr %tmp24, i64 8
  %tmp28 = load i64, ptr %tmp27, align 1
  store i64 %tmp28, ptr %tmp26, align 1
  %tmp29 = load i64, ptr %tmp26
  %tmp30 = insertvalue { i8, i64 } zeroinitializer, i8 %tmp25, 0
  %tmp31 = insertvalue { i8, i64 } %tmp30, i64 %tmp29, 1
  ret { i8, i64 } %tmp31
}

define internal void @"_I3:std10:std.sys.rt15:std.sys.rt.putc"(i8 %arg0) nounwind #0 {
bb0:
  %tmp0 = alloca %t4, align 1
  %l0 = alloca i8, align 1
  %l1 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l1
  %l2 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l2
  %l3 = alloca i8, align 1
  %l4 = alloca %t5, align 8
  store i8 %arg0, ptr %l0
  store %t4 zeroinitializer, ptr %tmp0
  %tmp1 = getelementptr %t4, ptr %tmp0, i64 0, i64 0
  store i8 0, ptr %tmp1
  %tmp2 = load %t4, ptr %tmp0
  store %t4 %tmp2, ptr %l1
  %tmp3 = load %t4, ptr %l1
  store %t4 %tmp3, ptr %l2
  %tmp4 = load i8, ptr %l0
  store i8 %tmp4, ptr %l3
  %tmp5 = load i8, ptr %l3
  %tmp6 = getelementptr %t4, ptr %l2, i64 0, i64 0
  store i8 %tmp5, ptr %tmp6
  %tmp7 = getelementptr %t4, ptr %l2, i64 0, i64 0
  %tmp8 = getelementptr i8, ptr %tmp7, i64 0
  %tmp9 = sub i64 1, 0
  %tmp10 = insertvalue %t5 zeroinitializer, ptr %tmp8, 0
  %tmp11 = insertvalue %t5 %tmp10, i64 %tmp9, 1
  store %t5 %tmp11, ptr %l4
  %tmp12 = load %t5, ptr %l4
  %tmp13 = extractvalue %t5 %tmp12, 0
  %tmp14 = extractvalue %t5 %tmp12, 1
  call void @"_I3:std10:std.sys.rt22:std.sys.rt.write_bytes"(ptr %tmp13, i64 %tmp14)
  br label %bb1
bb1:
  ret void
}

define internal void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr %arg0, i64 %arg1, i64 %arg2, i64 %arg3) nounwind cold noreturn #0 {
bb0:
  %l0 = alloca ptr, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca i64, align 8
  %l3 = alloca i64, align 8
  %l4 = alloca %t5, align 8
  %l5 = alloca i32, align 4
  %l6 = alloca i64, align 8
  %l7 = alloca i64, align 8
  %l8 = alloca i32, align 4
  %l9 = alloca i64, align 8
  %l10 = alloca i64, align 8
  %l11 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l11
  %l12 = alloca %t5, align 8
  %l13 = alloca %t5, align 8
  %l14 = alloca %t5, align 8
  %l15 = alloca i64, align 8
  %l16 = alloca i64, align 8
  %l17 = alloca i64, align 8
  %l18 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l18
  %l19 = alloca i8, align 1
  %l20 = alloca i8, align 1
  %l21 = alloca i8, align 1
  %l22 = alloca i1, align 1
  %l23 = alloca i1, align 1
  %l24 = alloca i1, align 1
  %l25 = alloca i64, align 8
  %l26 = alloca i1, align 1
  %l27 = alloca i1, align 1
  %l28 = alloca i1, align 1
  %l29 = alloca i64, align 8
  %l30 = alloca i64, align 8
  %l31 = alloca i8, align 1
  %l32 = alloca i1, align 1
  %l33 = alloca i1, align 1
  %l34 = alloca i1, align 1
  %l35 = alloca i64, align 8
  %l36 = alloca i1, align 1
  %l37 = alloca i1, align 1
  %l38 = alloca i1, align 1
  %l39 = alloca i64, align 8
  %l40 = alloca i64, align 8
  store ptr %arg0, ptr %l0
  store i64 %arg1, ptr %l1
  store i64 %arg2, ptr %l2
  store i64 %arg3, ptr %l3
  %tmp0 = load ptr, ptr %l0
  %tmp1 = getelementptr i8, ptr %tmp0, i64 8
  %tmp2 = load %t5, ptr %tmp1
  store %t5 %tmp2, ptr %l4
  %tmp3 = load %t5, ptr %l4
  %tmp4 = extractvalue %t5 %tmp3, 0
  %tmp5 = extractvalue %t5 %tmp3, 1
  call void @"_I3:std10:std.sys.rt22:std.sys.rt.write_bytes"(ptr %tmp4, i64 %tmp5)
  br label %bb1
bb1:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.putc"(i8 58)
  br label %bb2
bb2:
  %tmp6 = load ptr, ptr %l0
  %tmp7 = getelementptr i8, ptr %tmp6, i64 24
  %tmp8 = load i32, ptr %tmp7
  store i32 %tmp8, ptr %l5
  %tmp9 = load i32, ptr %l5
  %tmp10 = sext i32 %tmp9 to i64
  store i64 %tmp10, ptr %l6
  %tmp11 = load i64, ptr %l6
  store i64 %tmp11, ptr %l7
  %tmp12 = load i64, ptr %l7
  call void @"_I3:std10:std.sys.rt20:std.sys.rt.print_i64"(i64 %tmp12)
  br label %bb3
bb3:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.putc"(i8 58)
  br label %bb4
bb4:
  %tmp13 = load ptr, ptr %l0
  %tmp14 = getelementptr i8, ptr %tmp13, i64 28
  %tmp15 = load i32, ptr %tmp14
  store i32 %tmp15, ptr %l8
  %tmp16 = load i32, ptr %l8
  %tmp17 = sext i32 %tmp16 to i64
  store i64 %tmp17, ptr %l9
  %tmp18 = load i64, ptr %l9
  store i64 %tmp18, ptr %l10
  %tmp19 = load i64, ptr %l10
  call void @"_I3:std10:std.sys.rt20:std.sys.rt.print_i64"(i64 %tmp19)
  br label %bb5
bb5:
  %tmp20 = insertvalue %t5 zeroinitializer, ptr @str.9, 0
  %tmp21 = insertvalue %t5 %tmp20, i64 7, 1
  %tmp22 = extractvalue %t5 %tmp21, 0
  %tmp23 = extractvalue %t5 %tmp21, 1
  call void @"_I3:std10:std.sys.rt22:std.sys.rt.write_bytes"(ptr %tmp22, i64 %tmp23)
  br label %bb6
bb6:
  %tmp24 = load ptr, ptr %l0
  %tmp25 = getelementptr i8, ptr %tmp24, i64 0
  %tmp26 = load %t6, ptr %tmp25
  store %t6 %tmp26, ptr %l11
  %tmp27 = load %t6, ptr %l11
  %tmp28 = extractvalue %t6 %tmp27, 0
  %tmp29 = call %t5 @"_I3:std10:std.sys.rt20:std.sys.rt.code_text"(i8 %tmp28)
  store %t5 %tmp29, ptr %l12
  br label %bb7
bb7:
  %tmp30 = load %t5, ptr %l12
  store %t5 %tmp30, ptr %l13
  %tmp31 = load %t5, ptr %l13
  %tmp32 = extractvalue %t5 %tmp31, 0
  %tmp33 = extractvalue %t5 %tmp31, 1
  call void @"_I3:std10:std.sys.rt22:std.sys.rt.write_bytes"(ptr %tmp32, i64 %tmp33)
  br label %bb8
bb8:
  %tmp34 = insertvalue %t5 zeroinitializer, ptr @str.10, 0
  %tmp35 = insertvalue %t5 %tmp34, i64 2, 1
  %tmp36 = extractvalue %t5 %tmp35, 0
  %tmp37 = extractvalue %t5 %tmp35, 1
  call void @"_I3:std10:std.sys.rt22:std.sys.rt.write_bytes"(ptr %tmp36, i64 %tmp37)
  br label %bb9
bb9:
  %tmp38 = load ptr, ptr %l0
  %tmp39 = getelementptr i8, ptr %tmp38, i64 32
  %tmp40 = load %t5, ptr %tmp39
  store %t5 %tmp40, ptr %l14
  %tmp41 = load i64, ptr %l1
  store i64 %tmp41, ptr %l15
  %tmp42 = load i64, ptr %l2
  store i64 %tmp42, ptr %l16
  %tmp43 = load i64, ptr %l3
  store i64 %tmp43, ptr %l17
  %tmp44 = load %t5, ptr %l14
  %tmp45 = extractvalue %t5 %tmp44, 0
  %tmp46 = extractvalue %t5 %tmp44, 1
  %tmp47 = load i64, ptr %l15
  %tmp48 = load i64, ptr %l16
  %tmp49 = load i64, ptr %l17
  call void @"_I3:std10:std.sys.rt17:std.sys.rt.expand"(ptr %tmp45, i64 %tmp46, i64 %tmp47, i64 %tmp48, i64 %tmp49)
  br label %bb10
bb10:
  %tmp50 = load ptr, ptr %l0
  %tmp51 = getelementptr i8, ptr %tmp50, i64 0
  %tmp52 = load %t6, ptr %tmp51
  store %t6 %tmp52, ptr %l18
  %tmp53 = load %t6, ptr %l18
  %tmp55 = load i8, ptr %l18
  store i8 %tmp55, ptr %l19
  %tmp56 = load i8, ptr %l19
  store i8 %tmp56, ptr %l20
  %tmp57 = load i8, ptr %l20
  store i8 %tmp57, ptr %l21
  %tmp58 = load i8, ptr %l21
  %tmp59 = icmp eq i8 %tmp58, 7
  store i1 %tmp59, ptr %l22
  %tmp60 = load i1, ptr %l22
  store i1 %tmp60, ptr %l23
  %tmp61 = load i1, ptr %l23
  br i1 %tmp61, label %bb13, label %bb14
bb11:
  %tmp62 = insertvalue %t5 zeroinitializer, ptr @str.10, 0
  %tmp63 = insertvalue %t5 %tmp62, i64 2, 1
  %tmp64 = extractvalue %t5 %tmp63, 0
  %tmp65 = extractvalue %t5 %tmp63, 1
  call void @"_I3:std10:std.sys.rt22:std.sys.rt.write_bytes"(ptr %tmp64, i64 %tmp65)
  br label %bb17
bb12:
  br label %bb16
bb13:
  %tmp66 = load i64, ptr %l3
  store i64 %tmp66, ptr %l25
  %tmp67 = load i64, ptr %l25
  %tmp68 = icmp ne i64 %tmp67, 0
  store i1 %tmp68, ptr %l26
  %tmp69 = load i1, ptr %l26
  store i1 %tmp69, ptr %l27
  %tmp70 = load i1, ptr %l27
  store i1 %tmp70, ptr %l24
  br label %bb15
bb14:
  store i1 false, ptr %l24
  br label %bb15
bb15:
  %tmp71 = load i1, ptr %l24
  store i1 %tmp71, ptr %l28
  %tmp72 = load i1, ptr %l28
  br i1 %tmp72, label %bb11, label %bb12
bb16:
  %tmp73 = load i8, ptr %l20
  store i8 %tmp73, ptr %l31
  %tmp74 = load i8, ptr %l31
  %tmp75 = icmp eq i8 %tmp74, 8
  store i1 %tmp75, ptr %l32
  %tmp76 = load i1, ptr %l32
  store i1 %tmp76, ptr %l33
  %tmp77 = load i1, ptr %l33
  br i1 %tmp77, label %bb21, label %bb22
bb17:
  %tmp78 = load i64, ptr %l1
  store i64 %tmp78, ptr %l29
  %tmp79 = load i64, ptr %l2
  store i64 %tmp79, ptr %l30
  %tmp80 = load i64, ptr %l29
  %tmp81 = load i64, ptr %l30
  call void @"_I3:std10:std.sys.rt19:std.sys.rt.write_at"(i64 %tmp80, i64 %tmp81)
  br label %bb18
bb18:
  br label %bb16
bb19:
  %tmp82 = load i64, ptr %l1
  store i64 %tmp82, ptr %l39
  %tmp83 = load i64, ptr %l2
  store i64 %tmp83, ptr %l40
  %tmp84 = load i64, ptr %l39
  %tmp85 = load i64, ptr %l40
  call void @"_I3:std10:std.sys.rt19:std.sys.rt.write_at"(i64 %tmp84, i64 %tmp85)
  br label %bb25
bb20:
  br label %bb24
bb21:
  %tmp86 = load i64, ptr %l2
  store i64 %tmp86, ptr %l35
  %tmp87 = load i64, ptr %l35
  %tmp88 = icmp sgt i64 %tmp87, 0
  store i1 %tmp88, ptr %l36
  %tmp89 = load i1, ptr %l36
  store i1 %tmp89, ptr %l37
  %tmp90 = load i1, ptr %l37
  store i1 %tmp90, ptr %l34
  br label %bb23
bb22:
  store i1 false, ptr %l34
  br label %bb23
bb23:
  %tmp91 = load i1, ptr %l34
  store i1 %tmp91, ptr %l38
  %tmp92 = load i1, ptr %l38
  br i1 %tmp92, label %bb19, label %bb20
bb24:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.putc"(i8 10)
  br label %bb26
bb25:
  br label %bb24
bb26:
  call void @"_I3:std13:std.sys.macos21:std.sys.macos.os_exit"(i32 101)
  br label %bb27
bb27:
  unreachable
}

define internal void @"_I3:std10:std.sys.rt16:std.sys.rt.start"(i32 %arg0, ptr %arg1, ptr %arg2, ptr %arg3) nounwind cold noreturn #0 {
bb0:
  %tmp37 = alloca %t17, align 1
  %tmp40 = alloca %t18, align 8
  %l0 = alloca i32, align 4
  %l1 = alloca ptr, align 8
  %l2 = alloca ptr, align 8
  %l3 = alloca ptr, align 8
  %l4 = alloca i32, align 4
  %l5 = alloca ptr, align 8
  %l6 = alloca %t7, align 8
  %l7 = alloca %t7, align 8
  %l8 = alloca %t8, align 1
  %l9 = alloca %t8, align 1
  %l10 = alloca %t9, align 1
  %l11 = alloca %t9, align 1
  %l12 = alloca %t10, align 1
  %l13 = alloca %t10, align 1
  %l14 = alloca %t11, align 1
  %l15 = alloca %t11, align 1
  %l16 = alloca %t12, align 1
  %l17 = alloca %t12, align 1
  %l18 = alloca %t13, align 1
  %l19 = alloca %t13, align 1
  %l20 = alloca %t14, align 1
  %l21 = alloca %t14, align 1
  %l22 = alloca %t15, align 1
  %l23 = alloca %t15, align 1
  %l24 = alloca %t16, align 1
  %l25 = alloca %t16, align 1
  %l26 = alloca %t16, align 1
  %l27 = alloca %t16, align 1
  %l28 = alloca %t7, align 8
  %l29 = alloca ptr, align 8
  %l30 = alloca ptr, align 8
  %l31 = alloca %t9, align 1
  %l32 = alloca %t10, align 1
  %l33 = alloca ptr, align 8
  %l34 = alloca ptr, align 8
  %l35 = alloca ptr, align 8
  %l36 = alloca ptr, align 8
  %l37 = alloca ptr, align 8
  %l38 = alloca ptr, align 8
  %l39 = alloca ptr, align 8
  %l40 = alloca ptr, align 8
  %l41 = alloca ptr, align 8
  %l42 = alloca ptr, align 8
  %l43 = alloca ptr, align 8
  %l44 = alloca ptr, align 8
  %l45 = alloca ptr, align 8
  %l46 = alloca ptr, align 8
  %l47 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l47
  %l48 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l48
  %l49 = alloca %t18, align 8
  %l50 = alloca %t18, align 8
  %l51 = alloca ptr, align 8
  %l52 = alloca ptr, align 8
  %l53 = alloca i32, align 4
  %l54 = alloca i32, align 4
  %l55 = alloca i32, align 4
  store i32 %arg0, ptr %l0
  store ptr %arg1, ptr %l1
  store ptr %arg2, ptr %l2
  store ptr %arg3, ptr %l3
  %tmp0 = load i32, ptr %l0
  store i32 %tmp0, ptr %l4
  %tmp1 = load ptr, ptr %l1
  store ptr %tmp1, ptr %l5
  %tmp2 = load i32, ptr %l4
  %tmp3 = load ptr, ptr %l5
  %tmp4 = call %t7 @"_I3:std10:std.sys.rt21:std.sys.rt.args_table"(i32 %tmp2, ptr %tmp3)
  store %t7 %tmp4, ptr %l6
  br label %bb1
bb1:
  %tmp5 = load %t7, ptr %l6
  store %t7 %tmp5, ptr %l7
  %tmp6 = call %t8 @"_I3:std3:std15:std.Vars[].init10:std.Vars[]"()
  store %t8 %tmp6, ptr %l8
  br label %bb2
bb2:
  %tmp7 = load %t8, ptr %l8
  store %t8 %tmp7, ptr %l9
  %tmp8 = call %t9 @"_I3:std7:std.mem19:std.mem.Heap[].init14:std.mem.Heap[]"()
  store %t9 %tmp8, ptr %l10
  br label %bb3
bb3:
  %tmp9 = load %t9, ptr %l10
  store %t9 %tmp9, ptr %l11
  %tmp10 = call %t10 @"_I3:std7:std.mem17:std.mem.Vm[].init12:std.mem.Vm[]"()
  store %t10 %tmp10, ptr %l12
  br label %bb4
bb4:
  %tmp11 = load %t10, ptr %l12
  store %t10 %tmp11, ptr %l13
  %tmp12 = call %t11 @"_I3:std6:std.fs16:std.fs.Fs[].init11:std.fs.Fs[]"()
  store %t11 %tmp12, ptr %l14
  br label %bb5
bb5:
  %tmp13 = load %t11, ptr %l14
  store %t11 %tmp13, ptr %l15
  %tmp14 = call %t12 @"_I3:std7:std.net18:std.net.Net[].init13:std.net.Net[]"()
  store %t12 %tmp14, ptr %l16
  br label %bb6
bb6:
  %tmp15 = load %t12, ptr %l16
  store %t12 %tmp15, ptr %l17
  %tmp16 = call %t13 @"_I3:std8:std.time21:std.time.Clock[].init16:std.time.Clock[]"()
  store %t13 %tmp16, ptr %l18
  br label %bb7
bb7:
  %tmp17 = load %t13, ptr %l18
  store %t13 %tmp17, ptr %l19
  %tmp18 = call %t14 @"_I3:std8:std.rand20:std.rand.Rand[].init15:std.rand.Rand[]"()
  store %t14 %tmp18, ptr %l20
  br label %bb8
bb8:
  %tmp19 = load %t14, ptr %l20
  store %t14 %tmp19, ptr %l21
  %tmp20 = call %t15 @"_I3:std6:std.io20:std.io.Reader[].init15:std.io.Reader[]"()
  store %t15 %tmp20, ptr %l22
  br label %bb9
bb9:
  %tmp21 = load %t15, ptr %l22
  store %t15 %tmp21, ptr %l23
  %tmp22 = call %t16 @"_I3:std6:std.io20:std.io.Writer[].init15:std.io.Writer[]"()
  store %t16 %tmp22, ptr %l24
  br label %bb10
bb10:
  %tmp23 = load %t16, ptr %l24
  store %t16 %tmp23, ptr %l25
  %tmp24 = call %t16 @"_I3:std6:std.io20:std.io.Writer[].init15:std.io.Writer[]"()
  store %t16 %tmp24, ptr %l26
  br label %bb11
bb11:
  %tmp25 = load %t16, ptr %l26
  store %t16 %tmp25, ptr %l27
  %tmp26 = load %t7, ptr %l7
  store %t7 %tmp26, ptr %l28
  store ptr %l9, ptr %l29
  %tmp27 = load ptr, ptr %l29
  store ptr %tmp27, ptr %l30
  %tmp28 = load %t9, ptr %l11
  store %t9 %tmp28, ptr %l31
  %tmp29 = load %t10, ptr %l13
  store %t10 %tmp29, ptr %l32
  store ptr %l15, ptr %l33
  %tmp30 = load ptr, ptr %l33
  store ptr %tmp30, ptr %l34
  store ptr %l17, ptr %l35
  %tmp31 = load ptr, ptr %l35
  store ptr %tmp31, ptr %l36
  store ptr %l19, ptr %l37
  %tmp32 = load ptr, ptr %l37
  store ptr %tmp32, ptr %l38
  store ptr %l21, ptr %l39
  %tmp33 = load ptr, ptr %l39
  store ptr %tmp33, ptr %l40
  store ptr %l23, ptr %l41
  %tmp34 = load ptr, ptr %l41
  store ptr %tmp34, ptr %l42
  store ptr %l25, ptr %l43
  %tmp35 = load ptr, ptr %l43
  store ptr %tmp35, ptr %l44
  store ptr %l27, ptr %l45
  %tmp36 = load ptr, ptr %l45
  store ptr %tmp36, ptr %l46
  store %t17 zeroinitializer, ptr %tmp37
  store i8 0, ptr %tmp37
  %tmp38 = load %t17, ptr %tmp37
  store %t17 %tmp38, ptr %l47
  %tmp39 = load %t17, ptr %l47
  store %t17 %tmp39, ptr %l48
  store %t18 zeroinitializer, ptr %tmp40
  %tmp41 = load %t7, ptr %l28
  %tmp42 = getelementptr i8, ptr %tmp40, i64 0
  store %t7 %tmp41, ptr %tmp42
  %tmp43 = load ptr, ptr %l30
  %tmp44 = getelementptr i8, ptr %tmp40, i64 16
  store ptr %tmp43, ptr %tmp44
  %tmp45 = load %t9, ptr %l31
  %tmp46 = getelementptr i8, ptr %tmp40, i64 24
  store %t9 %tmp45, ptr %tmp46
  %tmp47 = load %t10, ptr %l32
  %tmp48 = getelementptr i8, ptr %tmp40, i64 24
  store %t10 %tmp47, ptr %tmp48
  %tmp49 = load ptr, ptr %l34
  %tmp50 = getelementptr i8, ptr %tmp40, i64 24
  store ptr %tmp49, ptr %tmp50
  %tmp51 = load ptr, ptr %l36
  %tmp52 = getelementptr i8, ptr %tmp40, i64 32
  store ptr %tmp51, ptr %tmp52
  %tmp53 = load ptr, ptr %l38
  %tmp54 = getelementptr i8, ptr %tmp40, i64 40
  store ptr %tmp53, ptr %tmp54
  %tmp55 = load ptr, ptr %l40
  %tmp56 = getelementptr i8, ptr %tmp40, i64 48
  store ptr %tmp55, ptr %tmp56
  %tmp57 = load ptr, ptr %l42
  %tmp58 = getelementptr i8, ptr %tmp40, i64 56
  store ptr %tmp57, ptr %tmp58
  %tmp59 = load ptr, ptr %l44
  %tmp60 = getelementptr i8, ptr %tmp40, i64 64
  store ptr %tmp59, ptr %tmp60
  %tmp61 = load ptr, ptr %l46
  %tmp62 = getelementptr i8, ptr %tmp40, i64 72
  store ptr %tmp61, ptr %tmp62
  %tmp63 = load %t17, ptr %l48
  %tmp64 = getelementptr i8, ptr %tmp40, i64 80
  store %t17 %tmp63, ptr %tmp64
  %tmp65 = load %t18, ptr %tmp40
  store %t18 %tmp65, ptr %l49
  %tmp66 = load %t18, ptr %l49
  store %t18 %tmp66, ptr %l50
  %tmp67 = load ptr, ptr %l3
  store ptr %tmp67, ptr %l51
  store ptr %l50, ptr %l52
  %tmp68 = load ptr, ptr %l51
  %tmp69 = load ptr, ptr %l52
  %tmp70 = call i32 %tmp68(ptr %tmp69)
  store i32 %tmp70, ptr %l53
  br label %bb12
bb12:
  %tmp71 = load i32, ptr %l53
  store i32 %tmp71, ptr %l54
  %tmp72 = load i32, ptr %l54
  store i32 %tmp72, ptr %l55
  %tmp73 = load i32, ptr %l55
  call void @"_I3:std13:std.sys.macos21:std.sys.macos.os_exit"(i32 %tmp73)
  br label %bb13
bb13:
  unreachable
}

define internal void @"_I3:std10:std.sys.rt17:std.sys.rt.expand"(ptr %arg0, i64 %arg1, i64 %arg2, i64 %arg3, i64 %arg4) nounwind #0 {
bb0:
  %l0 = alloca %t5, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca i64, align 8
  %l3 = alloca i64, align 8
  %l4 = alloca i64, align 8
  %l5 = alloca i64, align 8
  %l6 = alloca i64, align 8
  %l7 = alloca i64, align 8
  %l8 = alloca i1, align 1
  %l9 = alloca i1, align 1
  %l10 = alloca i64, align 8
  %l11 = alloca i8, align 1
  %l12 = alloca i8, align 1
  %l13 = alloca i1, align 1
  %l14 = alloca i1, align 1
  %l15 = alloca i64, align 8
  %l16 = alloca i8, align 1
  %l17 = alloca i1, align 1
  %l18 = alloca i1, align 1
  %l19 = alloca i64, align 8
  %l20 = alloca i8, align 1
  %l21 = alloca i1, align 1
  %l22 = alloca i1, align 1
  %l23 = alloca i64, align 8
  %l24 = alloca i8, align 1
  %l25 = alloca i64, align 8
  %l26 = alloca i64, align 8
  %tmp0 = insertvalue %t5 zeroinitializer, ptr %arg0, 0
  %tmp1 = insertvalue %t5 %tmp0, i64 %arg1, 1
  store %t5 %tmp1, ptr %l0
  store i64 %arg2, ptr %l1
  store i64 %arg3, ptr %l2
  store i64 %arg4, ptr %l3
  store i64 0, ptr %l4
  br label %bb1
bb1:
  %tmp2 = load i64, ptr %l4
  store i64 %tmp2, ptr %l5
  %tmp3 = getelementptr i8, ptr %l0, i64 8
  %tmp4 = load i64, ptr %tmp3
  store i64 %tmp4, ptr %l6
  %tmp5 = load i64, ptr %l6
  store i64 %tmp5, ptr %l7
  %tmp6 = load i64, ptr %l5
  %tmp7 = load i64, ptr %l7
  %tmp8 = icmp slt i64 %tmp6, %tmp7
  store i1 %tmp8, ptr %l8
  %tmp9 = load i1, ptr %l8
  store i1 %tmp9, ptr %l9
  %tmp10 = load i1, ptr %l9
  br i1 %tmp10, label %bb3, label %bb4
bb2:
  ret void
bb3:
  %tmp11 = load i64, ptr %l4
  store i64 %tmp11, ptr %l10
  %tmp12 = load i64, ptr %l10
  %tmp14 = load ptr, ptr %l0
  %tmp13 = getelementptr i8, ptr %tmp14, i64 %tmp12
  %tmp15 = load i8, ptr %tmp13
  store i8 %tmp15, ptr %l11
  %tmp16 = load i8, ptr %l11
  store i8 %tmp16, ptr %l12
  %tmp17 = load i8, ptr %l12
  %tmp18 = icmp eq i8 %tmp17, 1
  store i1 %tmp18, ptr %l13
  %tmp19 = load i1, ptr %l13
  store i1 %tmp19, ptr %l14
  %tmp20 = load i1, ptr %l14
  br i1 %tmp20, label %bb5, label %bb6
bb4:
  br label %bb2
bb5:
  %tmp21 = load i64, ptr %l1
  store i64 %tmp21, ptr %l15
  %tmp22 = load i64, ptr %l15
  call void @"_I3:std10:std.sys.rt20:std.sys.rt.print_i64"(i64 %tmp22)
  br label %bb8
bb6:
  %tmp23 = load i8, ptr %l11
  store i8 %tmp23, ptr %l16
  %tmp24 = load i8, ptr %l16
  %tmp25 = icmp eq i8 %tmp24, 2
  store i1 %tmp25, ptr %l17
  %tmp26 = load i1, ptr %l17
  store i1 %tmp26, ptr %l18
  %tmp27 = load i1, ptr %l18
  br i1 %tmp27, label %bb9, label %bb10
bb7:
  %tmp28 = load i64, ptr %l4
  store i64 %tmp28, ptr %l25
  %tmp29 = load i64, ptr %l25
  %tmp30 = add i64 %tmp29, 1
  store i64 %tmp30, ptr %l26
  %tmp31 = load i64, ptr %l26
  store i64 %tmp31, ptr %l4
  br label %bb1
bb8:
  br label %bb7
bb9:
  %tmp32 = load i64, ptr %l2
  store i64 %tmp32, ptr %l19
  %tmp33 = load i64, ptr %l19
  call void @"_I3:std10:std.sys.rt20:std.sys.rt.print_i64"(i64 %tmp33)
  br label %bb12
bb10:
  %tmp34 = load i8, ptr %l11
  store i8 %tmp34, ptr %l20
  %tmp35 = load i8, ptr %l20
  %tmp36 = icmp eq i8 %tmp35, 3
  store i1 %tmp36, ptr %l21
  %tmp37 = load i1, ptr %l21
  store i1 %tmp37, ptr %l22
  %tmp38 = load i1, ptr %l22
  br i1 %tmp38, label %bb13, label %bb14
bb11:
  br label %bb7
bb12:
  br label %bb11
bb13:
  %tmp39 = load i64, ptr %l3
  store i64 %tmp39, ptr %l23
  %tmp40 = load i64, ptr %l23
  call void @"_I3:std10:std.sys.rt20:std.sys.rt.print_i64"(i64 %tmp40)
  br label %bb16
bb14:
  %tmp41 = load i8, ptr %l11
  store i8 %tmp41, ptr %l24
  %tmp42 = load i8, ptr %l24
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.putc"(i8 %tmp42)
  br label %bb17
bb15:
  br label %bb11
bb16:
  br label %bb15
bb17:
  br label %bb15
}

define internal i64 @"_I3:std10:std.sys.rt18:std.sys.rt.ptr_add"(i64 %arg0, i64 %arg1) nounwind #0 {
bb0:
  %l0 = alloca i64, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca i64, align 8
  %l3 = alloca i64, align 8
  %l4 = alloca i64, align 8
  %l5 = alloca i64, align 8
  store i64 %arg0, ptr %l0
  store i64 %arg1, ptr %l1
  %tmp0 = load i64, ptr %l0
  store i64 %tmp0, ptr %l2
  %tmp1 = load i64, ptr %l1
  store i64 %tmp1, ptr %l3
  %tmp2 = load i64, ptr %l2
  %tmp3 = load i64, ptr %l3
  %tmp4 = add i64 %tmp2, %tmp3
  store i64 %tmp4, ptr %l4
  %tmp5 = load i64, ptr %l4
  store i64 %tmp5, ptr %l5
  %tmp6 = load i64, ptr %l5
  ret i64 %tmp6
}

define internal void @"_I3:std10:std.sys.rt19:std.sys.rt.write_at"(i64 %arg0, i64 %arg1) nounwind #0 {
bb0:
  %l0 = alloca i64, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca i64, align 8
  %l3 = alloca i64, align 8
  %l4 = alloca i64, align 8
  %l5 = alloca i64, align 8
  %l6 = alloca ptr, align 8
  %l7 = alloca ptr, align 8
  %l8 = alloca ptr, align 8
  %l9 = alloca ptr, align 8
  %l10 = alloca ptr, align 8
  %l11 = alloca ptr, align 8
  %l12 = alloca i64, align 8
  %l13 = alloca i64, align 8
  store i64 %arg0, ptr %l0
  store i64 %arg1, ptr %l1
  %tmp0 = load i64, ptr %l0
  store i64 %tmp0, ptr %l2
  %tmp1 = load i64, ptr %l2
  store i64 %tmp1, ptr %l3
  %tmp2 = load i64, ptr %l3
  store i64 %tmp2, ptr %l4
  %tmp3 = load i64, ptr %l4
  store i64 %tmp3, ptr %l5
  %tmp4 = load i64, ptr %l5
  %tmp5 = inttoptr i64 %tmp4 to ptr
  store ptr %tmp5, ptr %l6
  %tmp6 = load ptr, ptr %l6
  store ptr %tmp6, ptr %l7
  %tmp7 = load ptr, ptr %l7
  store ptr %tmp7, ptr %l8
  %tmp8 = load ptr, ptr %l8
  store ptr %tmp8, ptr %l9
  %tmp10 = load ptr, ptr %l9
  store ptr %tmp10, ptr %l10
  %tmp11 = load ptr, ptr %l10
  store ptr %tmp11, ptr %l11
  %tmp12 = load i64, ptr %l1
  store i64 %tmp12, ptr %l12
  %tmp13 = load ptr, ptr %l11
  %tmp14 = load i64, ptr %l12
  %tmp15 = call i64 @"_I3:std13:std.sys.macos22:std.sys.macos.os_write"(i32 2, ptr %tmp13, i64 %tmp14)
  store i64 %tmp15, ptr %l13
  br label %bb1
bb1:
  ret void
}

define internal i64 @"_I3:std10:std.sys.rt20:std.sys.rt.c_str_len"(ptr %arg0) nounwind #0 {
bb0:
  %l0 = alloca ptr, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca ptr, align 8
  %l3 = alloca i64, align 8
  %l4 = alloca i64, align 8
  %l5 = alloca i64, align 8
  %l6 = alloca i64, align 8
  %l7 = alloca i64, align 8
  %l8 = alloca i64, align 8
  %l9 = alloca i64, align 8
  %l10 = alloca i64, align 8
  %l11 = alloca i64, align 8
  %l12 = alloca ptr, align 8
  %l13 = alloca ptr, align 8
  %l14 = alloca i8, align 1
  %l15 = alloca i8, align 1
  %l16 = alloca i1, align 1
  %l17 = alloca i1, align 1
  %l18 = alloca i64, align 8
  %l19 = alloca i64, align 8
  %l20 = alloca i64, align 8
  store ptr %arg0, ptr %l0
  store i64 0, ptr %l1
  br label %bb1
bb1:
  br i1 true, label %bb3, label %bb4
bb2:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.9, i64 0, i64 0, i64 0)
  unreachable
bb3:
  %tmp0 = load ptr, ptr %l0
  store ptr %tmp0, ptr %l2
  %tmp1 = load ptr, ptr %l2
  %tmp2 = ptrtoint ptr %tmp1 to i64
  store i64 %tmp2, ptr %l3
  %tmp3 = load i64, ptr %l3
  store i64 %tmp3, ptr %l4
  %tmp4 = load i64, ptr %l1
  store i64 %tmp4, ptr %l5
  %tmp5 = load i64, ptr %l5
  store i64 %tmp5, ptr %l6
  %tmp6 = load i64, ptr %l6
  store i64 %tmp6, ptr %l7
  %tmp7 = load i64, ptr %l4
  store i64 %tmp7, ptr %l8
  %tmp8 = load i64, ptr %l7
  store i64 %tmp8, ptr %l9
  %tmp9 = load i64, ptr %l8
  %tmp10 = load i64, ptr %l9
  %tmp11 = call i64 @"_I3:std10:std.sys.rt18:std.sys.rt.ptr_add"(i64 %tmp9, i64 %tmp10)
  store i64 %tmp11, ptr %l10
  br label %bb5
bb4:
  br label %bb2
bb5:
  %tmp12 = load i64, ptr %l10
  store i64 %tmp12, ptr %l11
  %tmp13 = load i64, ptr %l11
  %tmp14 = inttoptr i64 %tmp13 to ptr
  store ptr %tmp14, ptr %l12
  %tmp15 = load ptr, ptr %l12
  store ptr %tmp15, ptr %l13
  %tmp16 = load ptr, ptr %l13
  %tmp17 = load i8, ptr %tmp16
  store i8 %tmp17, ptr %l14
  %tmp18 = load i8, ptr %l14
  store i8 %tmp18, ptr %l15
  %tmp19 = load i8, ptr %l15
  %tmp20 = icmp eq i8 %tmp19, 0
  store i1 %tmp20, ptr %l16
  %tmp21 = load i1, ptr %l16
  store i1 %tmp21, ptr %l17
  %tmp22 = load i1, ptr %l17
  br i1 %tmp22, label %bb6, label %bb7
bb6:
  %tmp23 = load i64, ptr %l1
  store i64 %tmp23, ptr %l18
  %tmp24 = load i64, ptr %l18
  ret i64 %tmp24
bb7:
  br label %bb8
bb8:
  %tmp25 = load i64, ptr %l1
  store i64 %tmp25, ptr %l19
  %tmp26 = load i64, ptr %l19
  %tmp27 = add i64 %tmp26, 1
  store i64 %tmp27, ptr %l20
  %tmp28 = load i64, ptr %l19
  %tmp29 = call { i64, i1 } @llvm.sadd.with.overflow.i64(i64 %tmp28, i64 1)
  %tmp30 = extractvalue { i64, i1 } %tmp29, 1
  br i1 %tmp30, label %trap0, label %cont1
cont1:
  %tmp31 = load i64, ptr %l20
  store i64 %tmp31, ptr %l1
  br label %bb1
trap0:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.10, i64 0, i64 0, i64 0)
  unreachable
}

define internal %t5 @"_I3:std10:std.sys.rt20:std.sys.rt.code_text"(i8 %arg0) nounwind #0 {
bb0:
  %l0 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l0
  %l1 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l1
  %l2 = alloca i8, align 1
  %l3 = alloca i8, align 1
  %l4 = alloca i8, align 1
  %l5 = alloca i1, align 1
  %l6 = alloca i1, align 1
  %l7 = alloca i8, align 1
  %l8 = alloca i1, align 1
  %l9 = alloca i1, align 1
  %l10 = alloca i8, align 1
  %l11 = alloca i1, align 1
  %l12 = alloca i1, align 1
  %l13 = alloca i8, align 1
  %l14 = alloca i1, align 1
  %l15 = alloca i1, align 1
  %l16 = alloca i8, align 1
  %l17 = alloca i1, align 1
  %l18 = alloca i1, align 1
  %l19 = alloca i8, align 1
  %l20 = alloca i1, align 1
  %l21 = alloca i1, align 1
  %l22 = alloca i8, align 1
  %l23 = alloca i1, align 1
  %l24 = alloca i1, align 1
  %l25 = alloca i8, align 1
  %l26 = alloca i1, align 1
  %l27 = alloca i1, align 1
  %l28 = alloca i8, align 1
  %l29 = alloca i1, align 1
  %l30 = alloca i1, align 1
  %l31 = alloca i8, align 1
  %l32 = alloca i1, align 1
  %l33 = alloca i1, align 1
  %l34 = alloca i8, align 1
  %l35 = alloca i1, align 1
  %l36 = alloca i1, align 1
  %l37 = alloca i8, align 1
  %l38 = alloca i1, align 1
  %l39 = alloca i1, align 1
  %l40 = alloca i8, align 1
  %l41 = alloca i1, align 1
  %l42 = alloca i1, align 1
  %l43 = alloca i8, align 1
  %l44 = alloca i1, align 1
  %l45 = alloca i1, align 1
  %l46 = alloca i8, align 1
  %l47 = alloca i1, align 1
  %l48 = alloca i1, align 1
  %l49 = alloca i8, align 1
  %l50 = alloca i1, align 1
  %l51 = alloca i1, align 1
  %tmp0 = insertvalue %t6 zeroinitializer, i8 %arg0, 0
  store %t6 %tmp0, ptr %l0
  %tmp1 = load %t6, ptr %l0
  store %t6 %tmp1, ptr %l1
  %tmp2 = load %t6, ptr %l1
  %tmp4 = load i8, ptr %l1
  store i8 %tmp4, ptr %l2
  %tmp5 = load i8, ptr %l2
  store i8 %tmp5, ptr %l3
  %tmp6 = load i8, ptr %l3
  store i8 %tmp6, ptr %l4
  %tmp7 = load i8, ptr %l4
  %tmp8 = icmp eq i8 %tmp7, 0
  store i1 %tmp8, ptr %l5
  %tmp9 = load i1, ptr %l5
  store i1 %tmp9, ptr %l6
  %tmp10 = load i1, ptr %l6
  br i1 %tmp10, label %bb1, label %bb2
bb1:
  %tmp11 = insertvalue %t5 zeroinitializer, ptr @str.13, 0
  %tmp12 = insertvalue %t5 %tmp11, i64 3, 1
  ret %t5 %tmp12
bb2:
  br label %bb3
bb3:
  %tmp13 = load i8, ptr %l3
  store i8 %tmp13, ptr %l7
  %tmp14 = load i8, ptr %l7
  %tmp15 = icmp eq i8 %tmp14, 1
  store i1 %tmp15, ptr %l8
  %tmp16 = load i1, ptr %l8
  store i1 %tmp16, ptr %l9
  %tmp17 = load i1, ptr %l9
  br i1 %tmp17, label %bb4, label %bb5
bb4:
  %tmp18 = insertvalue %t5 zeroinitializer, ptr @str.14, 0
  %tmp19 = insertvalue %t5 %tmp18, i64 3, 1
  ret %t5 %tmp19
bb5:
  br label %bb6
bb6:
  %tmp20 = load i8, ptr %l3
  store i8 %tmp20, ptr %l10
  %tmp21 = load i8, ptr %l10
  %tmp22 = icmp eq i8 %tmp21, 2
  store i1 %tmp22, ptr %l11
  %tmp23 = load i1, ptr %l11
  store i1 %tmp23, ptr %l12
  %tmp24 = load i1, ptr %l12
  br i1 %tmp24, label %bb7, label %bb8
bb7:
  %tmp25 = insertvalue %t5 zeroinitializer, ptr @str.15, 0
  %tmp26 = insertvalue %t5 %tmp25, i64 3, 1
  ret %t5 %tmp26
bb8:
  br label %bb9
bb9:
  %tmp27 = load i8, ptr %l3
  store i8 %tmp27, ptr %l13
  %tmp28 = load i8, ptr %l13
  %tmp29 = icmp eq i8 %tmp28, 3
  store i1 %tmp29, ptr %l14
  %tmp30 = load i1, ptr %l14
  store i1 %tmp30, ptr %l15
  %tmp31 = load i1, ptr %l15
  br i1 %tmp31, label %bb10, label %bb11
bb10:
  %tmp32 = insertvalue %t5 zeroinitializer, ptr @str.16, 0
  %tmp33 = insertvalue %t5 %tmp32, i64 3, 1
  ret %t5 %tmp33
bb11:
  br label %bb12
bb12:
  %tmp34 = load i8, ptr %l3
  store i8 %tmp34, ptr %l16
  %tmp35 = load i8, ptr %l16
  %tmp36 = icmp eq i8 %tmp35, 4
  store i1 %tmp36, ptr %l17
  %tmp37 = load i1, ptr %l17
  store i1 %tmp37, ptr %l18
  %tmp38 = load i1, ptr %l18
  br i1 %tmp38, label %bb13, label %bb14
bb13:
  %tmp39 = insertvalue %t5 zeroinitializer, ptr @str.17, 0
  %tmp40 = insertvalue %t5 %tmp39, i64 3, 1
  ret %t5 %tmp40
bb14:
  br label %bb15
bb15:
  %tmp41 = load i8, ptr %l3
  store i8 %tmp41, ptr %l19
  %tmp42 = load i8, ptr %l19
  %tmp43 = icmp eq i8 %tmp42, 5
  store i1 %tmp43, ptr %l20
  %tmp44 = load i1, ptr %l20
  store i1 %tmp44, ptr %l21
  %tmp45 = load i1, ptr %l21
  br i1 %tmp45, label %bb16, label %bb17
bb16:
  %tmp46 = insertvalue %t5 zeroinitializer, ptr @str.18, 0
  %tmp47 = insertvalue %t5 %tmp46, i64 3, 1
  ret %t5 %tmp47
bb17:
  br label %bb18
bb18:
  %tmp48 = load i8, ptr %l3
  store i8 %tmp48, ptr %l22
  %tmp49 = load i8, ptr %l22
  %tmp50 = icmp eq i8 %tmp49, 6
  store i1 %tmp50, ptr %l23
  %tmp51 = load i1, ptr %l23
  store i1 %tmp51, ptr %l24
  %tmp52 = load i1, ptr %l24
  br i1 %tmp52, label %bb19, label %bb20
bb19:
  %tmp53 = insertvalue %t5 zeroinitializer, ptr @str.19, 0
  %tmp54 = insertvalue %t5 %tmp53, i64 3, 1
  ret %t5 %tmp54
bb20:
  br label %bb21
bb21:
  %tmp55 = load i8, ptr %l3
  store i8 %tmp55, ptr %l25
  %tmp56 = load i8, ptr %l25
  %tmp57 = icmp eq i8 %tmp56, 7
  store i1 %tmp57, ptr %l26
  %tmp58 = load i1, ptr %l26
  store i1 %tmp58, ptr %l27
  %tmp59 = load i1, ptr %l27
  br i1 %tmp59, label %bb22, label %bb23
bb22:
  %tmp60 = insertvalue %t5 zeroinitializer, ptr @str.20, 0
  %tmp61 = insertvalue %t5 %tmp60, i64 3, 1
  ret %t5 %tmp61
bb23:
  br label %bb24
bb24:
  %tmp62 = load i8, ptr %l3
  store i8 %tmp62, ptr %l28
  %tmp63 = load i8, ptr %l28
  %tmp64 = icmp eq i8 %tmp63, 8
  store i1 %tmp64, ptr %l29
  %tmp65 = load i1, ptr %l29
  store i1 %tmp65, ptr %l30
  %tmp66 = load i1, ptr %l30
  br i1 %tmp66, label %bb25, label %bb26
bb25:
  %tmp67 = insertvalue %t5 zeroinitializer, ptr @str.21, 0
  %tmp68 = insertvalue %t5 %tmp67, i64 3, 1
  ret %t5 %tmp68
bb26:
  br label %bb27
bb27:
  %tmp69 = load i8, ptr %l3
  store i8 %tmp69, ptr %l31
  %tmp70 = load i8, ptr %l31
  %tmp71 = icmp eq i8 %tmp70, 9
  store i1 %tmp71, ptr %l32
  %tmp72 = load i1, ptr %l32
  store i1 %tmp72, ptr %l33
  %tmp73 = load i1, ptr %l33
  br i1 %tmp73, label %bb28, label %bb29
bb28:
  %tmp74 = insertvalue %t5 zeroinitializer, ptr @str.22, 0
  %tmp75 = insertvalue %t5 %tmp74, i64 3, 1
  ret %t5 %tmp75
bb29:
  br label %bb30
bb30:
  %tmp76 = load i8, ptr %l3
  store i8 %tmp76, ptr %l34
  %tmp77 = load i8, ptr %l34
  %tmp78 = icmp eq i8 %tmp77, 10
  store i1 %tmp78, ptr %l35
  %tmp79 = load i1, ptr %l35
  store i1 %tmp79, ptr %l36
  %tmp80 = load i1, ptr %l36
  br i1 %tmp80, label %bb31, label %bb32
bb31:
  %tmp81 = insertvalue %t5 zeroinitializer, ptr @str.23, 0
  %tmp82 = insertvalue %t5 %tmp81, i64 3, 1
  ret %t5 %tmp82
bb32:
  br label %bb33
bb33:
  %tmp83 = load i8, ptr %l3
  store i8 %tmp83, ptr %l37
  %tmp84 = load i8, ptr %l37
  %tmp85 = icmp eq i8 %tmp84, 11
  store i1 %tmp85, ptr %l38
  %tmp86 = load i1, ptr %l38
  store i1 %tmp86, ptr %l39
  %tmp87 = load i1, ptr %l39
  br i1 %tmp87, label %bb34, label %bb35
bb34:
  %tmp88 = insertvalue %t5 zeroinitializer, ptr @str.24, 0
  %tmp89 = insertvalue %t5 %tmp88, i64 3, 1
  ret %t5 %tmp89
bb35:
  br label %bb36
bb36:
  %tmp90 = load i8, ptr %l3
  store i8 %tmp90, ptr %l40
  %tmp91 = load i8, ptr %l40
  %tmp92 = icmp eq i8 %tmp91, 12
  store i1 %tmp92, ptr %l41
  %tmp93 = load i1, ptr %l41
  store i1 %tmp93, ptr %l42
  %tmp94 = load i1, ptr %l42
  br i1 %tmp94, label %bb37, label %bb38
bb37:
  %tmp95 = insertvalue %t5 zeroinitializer, ptr @str.25, 0
  %tmp96 = insertvalue %t5 %tmp95, i64 3, 1
  ret %t5 %tmp96
bb38:
  br label %bb39
bb39:
  %tmp97 = load i8, ptr %l3
  store i8 %tmp97, ptr %l43
  %tmp98 = load i8, ptr %l43
  %tmp99 = icmp eq i8 %tmp98, 13
  store i1 %tmp99, ptr %l44
  %tmp100 = load i1, ptr %l44
  store i1 %tmp100, ptr %l45
  %tmp101 = load i1, ptr %l45
  br i1 %tmp101, label %bb40, label %bb41
bb40:
  %tmp102 = insertvalue %t5 zeroinitializer, ptr @str.26, 0
  %tmp103 = insertvalue %t5 %tmp102, i64 3, 1
  ret %t5 %tmp103
bb41:
  br label %bb42
bb42:
  %tmp104 = load i8, ptr %l3
  store i8 %tmp104, ptr %l46
  %tmp105 = load i8, ptr %l46
  %tmp106 = icmp eq i8 %tmp105, 14
  store i1 %tmp106, ptr %l47
  %tmp107 = load i1, ptr %l47
  store i1 %tmp107, ptr %l48
  %tmp108 = load i1, ptr %l48
  br i1 %tmp108, label %bb43, label %bb44
bb43:
  %tmp109 = insertvalue %t5 zeroinitializer, ptr @str.27, 0
  %tmp110 = insertvalue %t5 %tmp109, i64 3, 1
  ret %t5 %tmp110
bb44:
  br label %bb45
bb45:
  %tmp111 = load i8, ptr %l3
  store i8 %tmp111, ptr %l49
  %tmp112 = load i8, ptr %l49
  %tmp113 = icmp eq i8 %tmp112, 15
  store i1 %tmp113, ptr %l50
  %tmp114 = load i1, ptr %l50
  store i1 %tmp114, ptr %l51
  %tmp115 = load i1, ptr %l51
  br i1 %tmp115, label %bb46, label %bb47
bb46:
  %tmp116 = insertvalue %t5 zeroinitializer, ptr @str.28, 0
  %tmp117 = insertvalue %t5 %tmp116, i64 3, 1
  ret %t5 %tmp117
bb47:
  br label %bb48
bb48:
  %tmp118 = insertvalue %t5 zeroinitializer, ptr @str.29, 0
  %tmp119 = insertvalue %t5 %tmp118, i64 3, 1
  ret %t5 %tmp119
}

define internal void @"_I3:std10:std.sys.rt20:std.sys.rt.print_i64"(i64 %arg0) nounwind #0 {
bb0:
  %l0 = alloca i64, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca i1, align 1
  %l3 = alloca i1, align 1
  %l4 = alloca i64, align 8
  %l5 = alloca i64, align 8
  %l6 = alloca i64, align 8
  %l7 = alloca i64, align 8
  %l8 = alloca i64, align 8
  %l9 = alloca i64, align 8
  %l10 = alloca i64, align 8
  %l11 = alloca i64, align 8
  store i64 %arg0, ptr %l0
  %tmp0 = load i64, ptr %l0
  store i64 %tmp0, ptr %l1
  %tmp1 = load i64, ptr %l1
  %tmp2 = icmp slt i64 %tmp1, 0
  store i1 %tmp2, ptr %l2
  %tmp3 = load i1, ptr %l2
  store i1 %tmp3, ptr %l3
  %tmp4 = load i1, ptr %l3
  br i1 %tmp4, label %bb1, label %bb2
bb1:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.putc"(i8 45)
  br label %bb4
bb2:
  %tmp5 = load i64, ptr %l0
  store i64 %tmp5, ptr %l9
  %tmp6 = load i64, ptr %l9
  store i64 %tmp6, ptr %l10
  %tmp7 = load i64, ptr %l10
  store i64 %tmp7, ptr %l11
  %tmp8 = load i64, ptr %l11
  call void @"_I3:std10:std.sys.rt20:std.sys.rt.print_u64"(i64 %tmp8)
  br label %bb6
bb3:
  ret void
bb4:
  %tmp9 = load i64, ptr %l0
  store i64 %tmp9, ptr %l4
  %tmp10 = load i64, ptr %l4
  %tmp11 = sub i64 0, %tmp10
  store i64 %tmp11, ptr %l5
  %tmp12 = load i64, ptr %l5
  store i64 %tmp12, ptr %l6
  %tmp13 = load i64, ptr %l6
  store i64 %tmp13, ptr %l7
  %tmp14 = load i64, ptr %l7
  store i64 %tmp14, ptr %l8
  %tmp15 = load i64, ptr %l8
  call void @"_I3:std10:std.sys.rt20:std.sys.rt.print_u64"(i64 %tmp15)
  br label %bb5
bb5:
  br label %bb3
bb6:
  br label %bb3
}

define internal void @"_I3:std10:std.sys.rt20:std.sys.rt.print_u64"(i64 %arg0) nounwind #0 {
bb0:
  %l0 = alloca i64, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca i1, align 1
  %l3 = alloca i1, align 1
  %l4 = alloca i64, align 8
  %l5 = alloca i64, align 8
  %l6 = alloca i64, align 8
  %l7 = alloca i64, align 8
  %l8 = alloca i64, align 8
  %l9 = alloca i64, align 8
  %l10 = alloca i8, align 1
  %l11 = alloca i8, align 1
  %l12 = alloca i8, align 1
  %l13 = alloca i8, align 1
  store i64 %arg0, ptr %l0
  %tmp0 = load i64, ptr %l0
  store i64 %tmp0, ptr %l1
  %tmp1 = load i64, ptr %l1
  %tmp2 = icmp uge i64 %tmp1, 10
  store i1 %tmp2, ptr %l2
  %tmp3 = load i1, ptr %l2
  store i1 %tmp3, ptr %l3
  %tmp4 = load i1, ptr %l3
  br i1 %tmp4, label %bb1, label %bb2
bb1:
  %tmp5 = load i64, ptr %l0
  store i64 %tmp5, ptr %l4
  %tmp6 = load i64, ptr %l4
  %tmp8 = icmp eq i64 10, 0
  %tmp9 = select i1 %tmp8, i64 1, i64 10
  %tmp7 = udiv i64 %tmp6, %tmp9
  store i64 %tmp7, ptr %l5
  %tmp10 = load i64, ptr %l5
  store i64 %tmp10, ptr %l6
  %tmp11 = load i64, ptr %l6
  call void @"_I3:std10:std.sys.rt20:std.sys.rt.print_u64"(i64 %tmp11)
  br label %bb4
bb2:
  br label %bb3
bb3:
  %tmp12 = load i64, ptr %l0
  store i64 %tmp12, ptr %l7
  %tmp13 = load i64, ptr %l7
  %tmp15 = icmp eq i64 10, 0
  %tmp16 = select i1 %tmp15, i64 1, i64 10
  %tmp14 = urem i64 %tmp13, %tmp16
  store i64 %tmp14, ptr %l8
  %tmp17 = load i64, ptr %l8
  store i64 %tmp17, ptr %l9
  %tmp18 = load i64, ptr %l9
  %tmp19 = trunc i64 %tmp18 to i8
  store i8 %tmp19, ptr %l10
  %tmp20 = load i8, ptr %l10
  store i8 %tmp20, ptr %l11
  %tmp21 = load i8, ptr %l11
  %tmp22 = add i8 %tmp21, 48
  store i8 %tmp22, ptr %l12
  %tmp23 = load i8, ptr %l11
  %tmp24 = call { i8, i1 } @llvm.uadd.with.overflow.i8(i8 %tmp23, i8 48)
  %tmp25 = extractvalue { i8, i1 } %tmp24, 1
  br i1 %tmp25, label %trap0, label %cont1
bb4:
  br label %bb3
bb5:
  ret void
cont1:
  %tmp26 = load i8, ptr %l12
  store i8 %tmp26, ptr %l13
  %tmp27 = load i8, ptr %l13
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.putc"(i8 %tmp27)
  br label %bb5
trap0:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.11, i64 0, i64 0, i64 0)
  unreachable
}

define internal %t7 @"_I3:std10:std.sys.rt21:std.sys.rt.args_table"(i32 %arg0, ptr %arg1) nounwind #0 {
bb0:
  %l0 = alloca i32, align 4
  %l1 = alloca ptr, align 8
  %l2 = alloca i32, align 4
  %l3 = alloca i64, align 8
  %l4 = alloca i64, align 8
  %l5 = alloca i64, align 8
  %l6 = alloca i64, align 8
  %l7 = alloca i64, align 8
  %l8 = alloca i64, align 8
  %l9 = alloca i64, align 8
  %l10 = alloca i64, align 8
  %l11 = alloca i64, align 8
  %l12 = alloca i64, align 8
  %l13 = alloca i1, align 1
  %l14 = alloca i1, align 1
  %l15 = alloca i64, align 8
  %l16 = alloca ptr, align 8
  %l17 = alloca ptr, align 8
  %l18 = alloca ptr, align 8
  %l19 = alloca i64, align 8
  %l20 = alloca i64, align 8
  %l21 = alloca i1, align 1
  %l22 = alloca i1, align 1
  %l24 = alloca ptr, align 8
  %l25 = alloca ptr, align 8
  %l26 = alloca ptr, align 8
  %l27 = alloca i64, align 8
  %l28 = alloca ptr, align 8
  %l29 = alloca i64, align 8
  %l30 = alloca i64, align 8
  %l31 = alloca ptr, align 8
  %l32 = alloca i64, align 8
  %l33 = alloca i64, align 8
  %l34 = alloca i64, align 8
  %l35 = alloca i64, align 8
  %l36 = alloca i64, align 8
  %l37 = alloca i64, align 8
  %l38 = alloca i64, align 8
  %l39 = alloca i64, align 8
  %l40 = alloca i64, align 8
  %l41 = alloca i64, align 8
  %l42 = alloca i64, align 8
  %l43 = alloca i64, align 8
  %l44 = alloca i1, align 1
  %l45 = alloca i1, align 1
  %l46 = alloca i64, align 8
  %l47 = alloca i64, align 8
  %l48 = alloca i64, align 8
  %l49 = alloca i64, align 8
  %l50 = alloca i64, align 8
  %l51 = alloca i64, align 8
  %l52 = alloca i64, align 8
  %l53 = alloca i64, align 8
  %l54 = alloca i64, align 8
  %l55 = alloca i64, align 8
  %l56 = alloca ptr, align 8
  %l57 = alloca ptr, align 8
  %l58 = alloca ptr, align 8
  %l59 = alloca ptr, align 8
  %l60 = alloca ptr, align 8
  %l61 = alloca ptr, align 8
  %l62 = alloca ptr, align 8
  %l63 = alloca ptr, align 8
  %l64 = alloca i64, align 8
  %l65 = alloca i64, align 8
  %l66 = alloca ptr, align 8
  %l67 = alloca i64, align 8
  %l68 = alloca %t5, align 8
  %l69 = alloca %t5, align 8
  %l70 = alloca i64, align 8
  %l71 = alloca i64, align 8
  %l72 = alloca i64, align 8
  %l73 = alloca i64, align 8
  %l74 = alloca i64, align 8
  %l75 = alloca i64, align 8
  %l76 = alloca i64, align 8
  %l77 = alloca ptr, align 8
  %l78 = alloca ptr, align 8
  %l79 = alloca %t5, align 8
  %l80 = alloca i64, align 8
  %l81 = alloca i64, align 8
  %l82 = alloca ptr, align 8
  %l83 = alloca ptr, align 8
  %l84 = alloca ptr, align 8
  %l85 = alloca ptr, align 8
  %l86 = alloca i64, align 8
  %l87 = alloca %t7, align 8
  %l88 = alloca %t7, align 8
  store i32 %arg0, ptr %l0
  store ptr %arg1, ptr %l1
  %tmp0 = load i32, ptr %l0
  store i32 %tmp0, ptr %l2
  %tmp1 = load i32, ptr %l2
  %tmp2 = sext i32 %tmp1 to i64
  store i64 %tmp2, ptr %l3
  %tmp3 = load i64, ptr %l3
  store i64 %tmp3, ptr %l4
  store i64 16, ptr %l5
  %tmp4 = load i64, ptr %l5
  store i64 %tmp4, ptr %l6
  %tmp5 = load i64, ptr %l4
  store i64 %tmp5, ptr %l7
  %tmp6 = load i64, ptr %l6
  store i64 %tmp6, ptr %l8
  %tmp7 = load i64, ptr %l7
  %tmp8 = load i64, ptr %l8
  %tmp9 = mul i64 %tmp7, %tmp8
  store i64 %tmp9, ptr %l9
  %tmp10 = load i64, ptr %l7
  %tmp11 = load i64, ptr %l8
  %tmp12 = call { i64, i1 } @llvm.smul.with.overflow.i64(i64 %tmp10, i64 %tmp11)
  %tmp13 = extractvalue { i64, i1 } %tmp12, 1
  br i1 %tmp13, label %trap0, label %cont1
bb1:
  store i64 1, ptr %l11
  br label %bb3
bb2:
  %tmp20 = load i64, ptr %l10
  store i64 %tmp20, ptr %l11
  br label %bb3
bb3:
  %tmp21 = load i64, ptr %l11
  store i64 %tmp21, ptr %l15
  %tmp22 = load i64, ptr %l15
  %tmp23 = call ptr @"_I3:std13:std.sys.macos20:std.sys.macos.os_map"(i64 %tmp22)
  store ptr %tmp23, ptr %l16
  br label %bb4
bb4:
  %tmp24 = load ptr, ptr %l16
  store ptr %tmp24, ptr %l17
  %tmp25 = load ptr, ptr %l17
  store ptr %tmp25, ptr %l18
  %tmp26 = load ptr, ptr %l18
  %tmp27 = ptrtoint ptr %tmp26 to i64
  store i64 %tmp27, ptr %l19
  %tmp28 = load i64, ptr %l19
  store i64 %tmp28, ptr %l20
  %tmp29 = load i64, ptr %l20
  %tmp30 = icmp eq i64 %tmp29, 18446744073709551615
  store i1 %tmp30, ptr %l21
  %tmp31 = load i1, ptr %l21
  store i1 %tmp31, ptr %l22
  %tmp32 = load i1, ptr %l22
  br i1 %tmp32, label %bb5, label %bb6
bb5:
  call void @"_I3:std10:std.sys.rt25:std.sys.rt.no_args_memory"()
  br label %bb8
bb6:
  br label %bb7
bb7:
  %tmp33 = load ptr, ptr %l17
  store ptr %tmp33, ptr %l24
  %tmp34 = load ptr, ptr %l24
  store ptr %tmp34, ptr %l25
  %tmp36 = load ptr, ptr %l25
  store ptr %tmp36, ptr %l26
  store i64 0, ptr %l27
  %tmp37 = load ptr, ptr %l1
  store ptr %tmp37, ptr %l28
  %tmp38 = load ptr, ptr %l28
  %tmp39 = ptrtoint ptr %tmp38 to i64
  store i64 %tmp39, ptr %l29
  %tmp40 = load i64, ptr %l29
  store i64 %tmp40, ptr %l30
  %tmp41 = load ptr, ptr %l26
  store ptr %tmp41, ptr %l31
  %tmp42 = load ptr, ptr %l31
  %tmp43 = ptrtoint ptr %tmp42 to i64
  store i64 %tmp43, ptr %l32
  %tmp44 = load i64, ptr %l32
  store i64 %tmp44, ptr %l33
  store i64 8, ptr %l34
  %tmp45 = load i64, ptr %l34
  store i64 %tmp45, ptr %l35
  %tmp46 = load i64, ptr %l35
  store i64 %tmp46, ptr %l36
  %tmp47 = load i64, ptr %l36
  store i64 %tmp47, ptr %l37
  store i64 16, ptr %l38
  %tmp48 = load i64, ptr %l38
  store i64 %tmp48, ptr %l39
  %tmp49 = load i64, ptr %l39
  store i64 %tmp49, ptr %l40
  %tmp50 = load i64, ptr %l40
  store i64 %tmp50, ptr %l41
  br label %bb9
bb8:
  br label %bb7
bb9:
  %tmp51 = load i64, ptr %l27
  store i64 %tmp51, ptr %l42
  %tmp52 = load i64, ptr %l4
  store i64 %tmp52, ptr %l43
  %tmp53 = load i64, ptr %l42
  %tmp54 = load i64, ptr %l43
  %tmp55 = icmp slt i64 %tmp53, %tmp54
  store i1 %tmp55, ptr %l44
  %tmp56 = load i1, ptr %l44
  store i1 %tmp56, ptr %l45
  %tmp57 = load i1, ptr %l45
  br i1 %tmp57, label %bb11, label %bb12
bb10:
  %tmp58 = load ptr, ptr %l26
  store ptr %tmp58, ptr %l82
  %tmp59 = load ptr, ptr %l82
  store ptr %tmp59, ptr %l83
  %tmp61 = load ptr, ptr %l83
  store ptr %tmp61, ptr %l84
  %tmp62 = load ptr, ptr %l84
  store ptr %tmp62, ptr %l85
  %tmp63 = load i64, ptr %l4
  store i64 %tmp63, ptr %l86
  %tmp64 = load ptr, ptr %l85
  %tmp65 = load i64, ptr %l86
  %tmp66 = insertvalue %t7 zeroinitializer, ptr %tmp64, 0
  %tmp67 = insertvalue %t7 %tmp66, i64 %tmp65, 1
  store %t7 %tmp67, ptr %l87
  %tmp68 = load %t7, ptr %l87
  store %t7 %tmp68, ptr %l88
  %tmp69 = load %t7, ptr %l88
  ret %t7 %tmp69
bb11:
  %tmp70 = load i64, ptr %l27
  store i64 %tmp70, ptr %l46
  %tmp71 = load i64, ptr %l46
  store i64 %tmp71, ptr %l47
  %tmp72 = load i64, ptr %l47
  store i64 %tmp72, ptr %l48
  %tmp73 = load i64, ptr %l30
  store i64 %tmp73, ptr %l49
  %tmp74 = load i64, ptr %l48
  store i64 %tmp74, ptr %l50
  %tmp75 = load i64, ptr %l37
  store i64 %tmp75, ptr %l51
  %tmp76 = load i64, ptr %l50
  %tmp77 = load i64, ptr %l51
  %tmp78 = mul i64 %tmp76, %tmp77
  store i64 %tmp78, ptr %l52
  %tmp79 = load i64, ptr %l52
  store i64 %tmp79, ptr %l53
  %tmp80 = load i64, ptr %l49
  %tmp81 = load i64, ptr %l53
  %tmp82 = call i64 @"_I3:std10:std.sys.rt18:std.sys.rt.ptr_add"(i64 %tmp80, i64 %tmp81)
  store i64 %tmp82, ptr %l54
  br label %bb13
bb12:
  br label %bb10
bb13:
  %tmp83 = load i64, ptr %l54
  store i64 %tmp83, ptr %l55
  %tmp84 = load i64, ptr %l55
  %tmp85 = inttoptr i64 %tmp84 to ptr
  store ptr %tmp85, ptr %l56
  %tmp86 = load ptr, ptr %l56
  store ptr %tmp86, ptr %l57
  %tmp87 = load ptr, ptr %l57
  %tmp88 = load ptr, ptr %tmp87
  store ptr %tmp88, ptr %l58
  %tmp89 = load ptr, ptr %l58
  store ptr %tmp89, ptr %l59
  %tmp90 = load ptr, ptr %l59
  store ptr %tmp90, ptr %l60
  %tmp91 = load ptr, ptr %l60
  store ptr %tmp91, ptr %l61
  %tmp93 = load ptr, ptr %l61
  store ptr %tmp93, ptr %l62
  %tmp94 = load ptr, ptr %l62
  store ptr %tmp94, ptr %l63
  %tmp95 = load ptr, ptr %l63
  %tmp96 = call i64 @"_I3:std10:std.sys.rt20:std.sys.rt.c_str_len"(ptr %tmp95)
  store i64 %tmp96, ptr %l64
  br label %bb14
bb14:
  %tmp97 = load i64, ptr %l64
  store i64 %tmp97, ptr %l65
  %tmp98 = load ptr, ptr %l62
  store ptr %tmp98, ptr %l66
  %tmp99 = load i64, ptr %l65
  store i64 %tmp99, ptr %l67
  %tmp100 = load ptr, ptr %l66
  %tmp101 = load i64, ptr %l67
  %tmp102 = insertvalue %t5 zeroinitializer, ptr %tmp100, 0
  %tmp103 = insertvalue %t5 %tmp102, i64 %tmp101, 1
  store %t5 %tmp103, ptr %l68
  %tmp104 = load %t5, ptr %l68
  store %t5 %tmp104, ptr %l69
  %tmp105 = load i64, ptr %l33
  store i64 %tmp105, ptr %l70
  %tmp106 = load i64, ptr %l48
  store i64 %tmp106, ptr %l71
  %tmp107 = load i64, ptr %l41
  store i64 %tmp107, ptr %l72
  %tmp108 = load i64, ptr %l71
  %tmp109 = load i64, ptr %l72
  %tmp110 = mul i64 %tmp108, %tmp109
  store i64 %tmp110, ptr %l73
  %tmp111 = load i64, ptr %l73
  store i64 %tmp111, ptr %l74
  %tmp112 = load i64, ptr %l70
  %tmp113 = load i64, ptr %l74
  %tmp114 = call i64 @"_I3:std10:std.sys.rt18:std.sys.rt.ptr_add"(i64 %tmp112, i64 %tmp113)
  store i64 %tmp114, ptr %l75
  br label %bb15
bb15:
  %tmp115 = load i64, ptr %l75
  store i64 %tmp115, ptr %l76
  %tmp116 = load i64, ptr %l76
  %tmp117 = inttoptr i64 %tmp116 to ptr
  store ptr %tmp117, ptr %l77
  %tmp118 = load ptr, ptr %l77
  store ptr %tmp118, ptr %l78
  %tmp119 = load %t5, ptr %l69
  store %t5 %tmp119, ptr %l79
  %tmp120 = load %t5, ptr %l79
  %tmp121 = load ptr, ptr %l78
  store %t5 %tmp120, ptr %tmp121
  %tmp122 = load i64, ptr %l27
  store i64 %tmp122, ptr %l80
  %tmp123 = load i64, ptr %l80
  %tmp124 = add i64 %tmp123, 1
  store i64 %tmp124, ptr %l81
  %tmp125 = load i64, ptr %l80
  %tmp126 = call { i64, i1 } @llvm.sadd.with.overflow.i64(i64 %tmp125, i64 1)
  %tmp127 = extractvalue { i64, i1 } %tmp126, 1
  br i1 %tmp127, label %trap2, label %cont3
cont1:
  %tmp14 = load i64, ptr %l9
  store i64 %tmp14, ptr %l10
  %tmp15 = load i64, ptr %l10
  store i64 %tmp15, ptr %l12
  %tmp16 = load i64, ptr %l12
  %tmp17 = icmp eq i64 %tmp16, 0
  store i1 %tmp17, ptr %l13
  %tmp18 = load i1, ptr %l13
  store i1 %tmp18, ptr %l14
  %tmp19 = load i1, ptr %l14
  br i1 %tmp19, label %bb1, label %bb2
cont3:
  %tmp128 = load i64, ptr %l81
  store i64 %tmp128, ptr %l27
  br label %bb9
trap0:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.12, i64 0, i64 0, i64 0)
  unreachable
trap2:
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr @site.13, i64 0, i64 0, i64 0)
  unreachable
}

define internal void @"_I3:std10:std.sys.rt22:std.sys.rt.write_bytes"(ptr %arg0, i64 %arg1) nounwind #0 {
bb0:
  %l0 = alloca %t5, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca i64, align 8
  %l3 = alloca i1, align 1
  %l4 = alloca i1, align 1
  %l5 = alloca ptr, align 8
  %l6 = alloca ptr, align 8
  %l7 = alloca ptr, align 8
  %l8 = alloca i64, align 8
  %l9 = alloca i64, align 8
  %l10 = alloca i64, align 8
  %tmp0 = insertvalue %t5 zeroinitializer, ptr %arg0, 0
  %tmp1 = insertvalue %t5 %tmp0, i64 %arg1, 1
  store %t5 %tmp1, ptr %l0
  %tmp2 = getelementptr i8, ptr %l0, i64 8
  %tmp3 = load i64, ptr %tmp2
  store i64 %tmp3, ptr %l1
  %tmp4 = load i64, ptr %l1
  store i64 %tmp4, ptr %l2
  %tmp5 = load i64, ptr %l2
  %tmp6 = icmp eq i64 %tmp5, 0
  store i1 %tmp6, ptr %l3
  %tmp7 = load i1, ptr %l3
  store i1 %tmp7, ptr %l4
  %tmp8 = load i1, ptr %l4
  br i1 %tmp8, label %bb1, label %bb2
bb1:
  ret void
bb2:
  br label %bb3
bb3:
  %tmp9 = load ptr, ptr %l0
  store ptr %tmp9, ptr %l5
  %tmp10 = load ptr, ptr %l5
  store ptr %tmp10, ptr %l6
  %tmp11 = load ptr, ptr %l6
  store ptr %tmp11, ptr %l7
  %tmp12 = getelementptr i8, ptr %l0, i64 8
  %tmp13 = load i64, ptr %tmp12
  store i64 %tmp13, ptr %l8
  %tmp14 = load i64, ptr %l8
  store i64 %tmp14, ptr %l9
  %tmp15 = load ptr, ptr %l7
  %tmp16 = load i64, ptr %l9
  %tmp17 = call i64 @"_I3:std13:std.sys.macos22:std.sys.macos.os_write"(i32 2, ptr %tmp15, i64 %tmp16)
  store i64 %tmp17, ptr %l10
  br label %bb4
bb4:
  ret void
}

define internal void @"_I3:std10:std.sys.rt25:std.sys.rt.no_args_memory"() nounwind cold noreturn #0 {
bb0:
  %tmp0 = alloca %t6, align 1
  %tmp3 = alloca %t19, align 8
  %l0 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l0
  %l1 = alloca [16 x i8], align 8
  store [16 x i8] zeroinitializer, ptr %l1
  %l2 = alloca %t19, align 8
  %l3 = alloca %t19, align 8
  %l4 = alloca ptr, align 8
  store %t6 zeroinitializer, ptr %tmp0
  store i8 11, ptr %tmp0
  %tmp1 = load %t6, ptr %tmp0
  store %t6 %tmp1, ptr %l0
  %tmp2 = load %t6, ptr %l0
  store %t6 %tmp2, ptr %l1
  store %t19 zeroinitializer, ptr %tmp3
  %tmp4 = load %t6, ptr %l1
  %tmp5 = getelementptr i8, ptr %tmp3, i64 0
  store %t6 %tmp4, ptr %tmp5
  %tmp6 = insertvalue %t5 zeroinitializer, ptr @str.31, 0
  %tmp7 = insertvalue %t5 %tmp6, i64 9, 1
  %tmp8 = getelementptr i8, ptr %tmp3, i64 8
  store %t5 %tmp7, ptr %tmp8
  %tmp9 = getelementptr i8, ptr %tmp3, i64 24
  store i32 0, ptr %tmp9
  %tmp10 = getelementptr i8, ptr %tmp3, i64 28
  store i32 0, ptr %tmp10
  %tmp11 = insertvalue %t5 zeroinitializer, ptr @str.32, 0
  %tmp12 = insertvalue %t5 %tmp11, i64 20, 1
  %tmp13 = getelementptr i8, ptr %tmp3, i64 32
  store %t5 %tmp12, ptr %tmp13
  %tmp14 = load %t19, ptr %tmp3
  store %t19 %tmp14, ptr %l2
  %tmp15 = load %t19, ptr %l2
  store %t19 %tmp15, ptr %l3
  store ptr %l3, ptr %l4
  %tmp16 = load ptr, ptr %l4
  call void @"_I3:std10:std.sys.rt15:std.sys.rt.trap"(ptr %tmp16, i64 0, i64 0, i64 0)
  br label %bb1
bb1:
  unreachable
}

define internal ptr @"_I3:std13:std.sys.macos20:std.sys.macos.os_map"(i64 %arg0) nounwind #0 {
bb0:
  %l0 = alloca i64, align 8
  %l1 = alloca i64, align 8
  %l2 = alloca i64, align 8
  %l3 = alloca ptr, align 8
  %l4 = alloca ptr, align 8
  %l5 = alloca i64, align 8
  %l6 = alloca ptr, align 8
  %l7 = alloca ptr, align 8
  store i64 %arg0, ptr %l0
  store i64 0, ptr %l1
  %tmp0 = load i64, ptr %l1
  store i64 %tmp0, ptr %l2
  %tmp1 = load i64, ptr %l2
  %tmp2 = inttoptr i64 %tmp1 to ptr
  store ptr %tmp2, ptr %l3
  %tmp3 = load ptr, ptr %l3
  store ptr %tmp3, ptr %l4
  %tmp4 = load i64, ptr %l0
  store i64 %tmp4, ptr %l5
  %tmp5 = load ptr, ptr %l4
  %tmp6 = load i64, ptr %l5
  %tmp7 = call ptr @mmap(ptr %tmp5, i64 %tmp6, i32 3, i32 4098, i32 -1, i64 0)
  store ptr %tmp7, ptr %l6
  br label %bb1
bb1:
  %tmp8 = load ptr, ptr %l6
  store ptr %tmp8, ptr %l7
  %tmp9 = load ptr, ptr %l7
  ret ptr %tmp9
}

define internal void @"_I3:std13:std.sys.macos21:std.sys.macos.os_exit"(i32 %arg0) nounwind cold noreturn #0 {
bb0:
  %l0 = alloca i32, align 4
  %l1 = alloca i32, align 4
  store i32 %arg0, ptr %l0
  %tmp0 = load i32, ptr %l0
  store i32 %tmp0, ptr %l1
  %tmp1 = load i32, ptr %l1
  call void @exit(i32 %tmp1)
  br label %bb1
bb1:
  unreachable
}

define internal i64 @"_I3:std13:std.sys.macos22:std.sys.macos.os_write"(i32 %arg0, ptr %arg1, i64 %arg2) nounwind #0 {
bb0:
  %l0 = alloca i32, align 4
  %l1 = alloca ptr, align 8
  %l2 = alloca i64, align 8
  %l3 = alloca i32, align 4
  %l4 = alloca ptr, align 8
  %l5 = alloca i64, align 8
  %l6 = alloca i64, align 8
  %l7 = alloca i64, align 8
  store i32 %arg0, ptr %l0
  store ptr %arg1, ptr %l1
  store i64 %arg2, ptr %l2
  %tmp0 = load i32, ptr %l0
  store i32 %tmp0, ptr %l3
  %tmp1 = load ptr, ptr %l1
  store ptr %tmp1, ptr %l4
  %tmp2 = load i64, ptr %l2
  store i64 %tmp2, ptr %l5
  %tmp3 = load i32, ptr %l3
  %tmp4 = load ptr, ptr %l4
  %tmp5 = load i64, ptr %l5
  %tmp6 = call i64 @write(i32 %tmp3, ptr %tmp4, i64 %tmp5)
  store i64 %tmp6, ptr %l6
  br label %bb1
bb1:
  %tmp7 = load i64, ptr %l6
  store i64 %tmp7, ptr %l7
  %tmp8 = load i64, ptr %l7
  ret i64 %tmp8
}

define internal %t8 @"_I3:std3:std15:std.Vars[].init10:std.Vars[]"() nounwind #0 {
bb0:
  %tmp0 = alloca %t8, align 1
  %l0 = alloca %t8, align 1
  %l1 = alloca %t8, align 1
  store %t8 zeroinitializer, ptr %tmp0
  %tmp1 = load %t8, ptr %tmp0
  store %t8 %tmp1, ptr %l0
  %tmp2 = load %t8, ptr %l0
  store %t8 %tmp2, ptr %l1
  %tmp3 = load %t8, ptr %l1
  ret %t8 %tmp3
}

define internal %t11 @"_I3:std6:std.fs16:std.fs.Fs[].init11:std.fs.Fs[]"() nounwind #0 {
bb0:
  %tmp0 = alloca %t11, align 1
  %l0 = alloca %t11, align 1
  %l1 = alloca %t11, align 1
  store %t11 zeroinitializer, ptr %tmp0
  %tmp1 = load %t11, ptr %tmp0
  store %t11 %tmp1, ptr %l0
  %tmp2 = load %t11, ptr %l0
  store %t11 %tmp2, ptr %l1
  %tmp3 = load %t11, ptr %l1
  ret %t11 %tmp3
}

define internal %t15 @"_I3:std6:std.io20:std.io.Reader[].init15:std.io.Reader[]"() nounwind #0 {
bb0:
  %tmp0 = alloca %t15, align 1
  %l0 = alloca %t15, align 1
  %l1 = alloca %t15, align 1
  store %t15 zeroinitializer, ptr %tmp0
  %tmp1 = load %t15, ptr %tmp0
  store %t15 %tmp1, ptr %l0
  %tmp2 = load %t15, ptr %l0
  store %t15 %tmp2, ptr %l1
  %tmp3 = load %t15, ptr %l1
  ret %t15 %tmp3
}

define internal %t16 @"_I3:std6:std.io20:std.io.Writer[].init15:std.io.Writer[]"() nounwind #0 {
bb0:
  %tmp0 = alloca %t16, align 1
  %l0 = alloca %t16, align 1
  %l1 = alloca %t16, align 1
  store %t16 zeroinitializer, ptr %tmp0
  %tmp1 = load %t16, ptr %tmp0
  store %t16 %tmp1, ptr %l0
  %tmp2 = load %t16, ptr %l0
  store %t16 %tmp2, ptr %l1
  %tmp3 = load %t16, ptr %l1
  ret %t16 %tmp3
}

define internal %t10 @"_I3:std7:std.mem17:std.mem.Vm[].init12:std.mem.Vm[]"() nounwind #0 {
bb0:
  %tmp0 = alloca %t10, align 1
  %l0 = alloca %t10, align 1
  %l1 = alloca %t10, align 1
  store %t10 zeroinitializer, ptr %tmp0
  %tmp1 = load %t10, ptr %tmp0
  store %t10 %tmp1, ptr %l0
  %tmp2 = load %t10, ptr %l0
  store %t10 %tmp2, ptr %l1
  %tmp3 = load %t10, ptr %l1
  ret %t10 %tmp3
}

define internal %t9 @"_I3:std7:std.mem19:std.mem.Heap[].init14:std.mem.Heap[]"() nounwind #0 {
bb0:
  %tmp0 = alloca %t9, align 1
  %l0 = alloca %t9, align 1
  %l1 = alloca %t9, align 1
  store %t9 zeroinitializer, ptr %tmp0
  %tmp1 = load %t9, ptr %tmp0
  store %t9 %tmp1, ptr %l0
  %tmp2 = load %t9, ptr %l0
  store %t9 %tmp2, ptr %l1
  %tmp3 = load %t9, ptr %l1
  ret %t9 %tmp3
}

define internal %t12 @"_I3:std7:std.net18:std.net.Net[].init13:std.net.Net[]"() nounwind #0 {
bb0:
  %tmp0 = alloca %t12, align 1
  %l0 = alloca %t12, align 1
  %l1 = alloca %t12, align 1
  store %t12 zeroinitializer, ptr %tmp0
  %tmp1 = load %t12, ptr %tmp0
  store %t12 %tmp1, ptr %l0
  %tmp2 = load %t12, ptr %l0
  store %t12 %tmp2, ptr %l1
  %tmp3 = load %t12, ptr %l1
  ret %t12 %tmp3
}

define internal %t14 @"_I3:std8:std.rand20:std.rand.Rand[].init15:std.rand.Rand[]"() nounwind #0 {
bb0:
  %tmp0 = alloca %t14, align 1
  %l0 = alloca %t14, align 1
  %l1 = alloca %t14, align 1
  store %t14 zeroinitializer, ptr %tmp0
  %tmp1 = load %t14, ptr %tmp0
  store %t14 %tmp1, ptr %l0
  %tmp2 = load %t14, ptr %l0
  store %t14 %tmp2, ptr %l1
  %tmp3 = load %t14, ptr %l1
  ret %t14 %tmp3
}

define internal %t13 @"_I3:std8:std.time21:std.time.Clock[].init16:std.time.Clock[]"() nounwind #0 {
bb0:
  %tmp0 = alloca %t13, align 1
  %l0 = alloca %t13, align 1
  %l1 = alloca %t13, align 1
  store %t13 zeroinitializer, ptr %tmp0
  %tmp1 = load %t13, ptr %tmp0
  store %t13 %tmp1, ptr %l0
  %tmp2 = load %t13, ptr %l0
  store %t13 %tmp2, ptr %l1
  %tmp3 = load %t13, ptr %l1
  ret %t13 %tmp3
}

define internal i32 @_I.entry(ptr %env) nounwind #0 {
entry:
  %r = call i8 @"_I1:t1:t6:t.main"(ptr %env)
  %status = zext i8 %r to i32
  ret i32 %status
}

define i32 @main(i32 %argc, ptr %argv, ptr %envp) nounwind #0 {
entry:
  call void @"_I3:std10:std.sys.rt16:std.sys.rt.start"(i32 %argc, ptr %argv, ptr %envp, ptr @_I.entry)
  unreachable
}

attributes #0 = { "probe-stack"="inline-asm" "stack-probe-size"="16384" }
