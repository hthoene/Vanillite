// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.extractor.golden;

import java.lang.classfile.ClassFile;
import java.lang.classfile.ClassModel;
import java.lang.classfile.ClassTransform;
import java.lang.classfile.CodeTransform;
import java.lang.classfile.MethodModel;
import java.lang.classfile.MethodTransform;
import java.lang.classfile.instruction.ReturnInstruction;
import java.lang.constant.ClassDesc;
import java.lang.constant.ConstantDescs;
import java.lang.constant.MethodTypeDesc;
import java.lang.instrument.ClassFileTransformer;
import java.lang.instrument.Instrumentation;
import java.security.ProtectionDomain;

/**
 * Java agent of the scenario recorder: makes Vanilla's randomness reproducible.
 *
 * <p>Vanilla seeds every {@code RandomSource.create()} (the random of every entity, of the level,
 * of shuffled behaviour lists) from {@code System.nanoTime()}. The agent rewrites two methods:
 *
 * <ul>
 *   <li>{@code RandomSupport.generateUniqueSeed()} returns {@link ParityHooks#uniqueSeed()}: a
 *       counter, so a recording is the same on every run.
 *   <li>the end of the {@code Entity} constructor calls {@link ParityHooks#entityCreated}, which
 *       reseeds the new entity's random from the scenario's entity seed stream (the subclass
 *       constructors run after it and see the seeded random). Vanillite seeds new entities from
 *       the same stream.
 * </ul>
 *
 * <p>Only Vanilla's randomness changes; no game logic does.
 */
public final class ParityAgent {
  private static final ClassDesc HOOKS = ClassDesc.of(ParityHooks.class.getName());

  private ParityAgent() {}

  public static void premain(String args, Instrumentation inst) {
    inst.addTransformer(
        new ClassFileTransformer() {
          @Override
          public byte[] transform(
              ClassLoader loader,
              String name,
              Class<?> redefined,
              ProtectionDomain domain,
              byte[] bytes) {
            try {
              return switch (name) {
                case "net/minecraft/world/level/levelgen/RandomSupport" -> uniqueSeed(bytes);
                case "net/minecraft/world/entity/Entity" -> entityCreated(bytes);
                default -> null;
              };
            } catch (RuntimeException e) {
              e.printStackTrace();
              throw e;
            }
          }
        });
  }

  /** {@code generateUniqueSeed()J}: {@code return ParityHooks.uniqueSeed();}. */
  private static byte[] uniqueSeed(byte[] bytes) {
    ClassFile cf = ClassFile.of();
    ClassModel model = cf.parse(bytes);
    ClassTransform t =
        ClassTransform.transformingMethods(
            m -> m.methodName().equalsString("generateUniqueSeed"),
            MethodTransform.transformingCode(
                CodeTransform.ofStateful(
                    () ->
                        new CodeTransform() {
                          private boolean done;

                          @Override
                          public void accept(
                              java.lang.classfile.CodeBuilder b,
                              java.lang.classfile.CodeElement e) {
                            if (!done) {
                              done = true;
                              b.invokestatic(
                                  HOOKS, "uniqueSeed", MethodTypeDesc.of(ConstantDescs.CD_long));
                              b.lreturn();
                            }
                            // The original code stays behind the return, unreachable.
                            b.with(e);
                          }
                        })));
    return cf.transformClass(model, t);
  }

  /** Every {@code Entity.<init>}: {@code ParityHooks.entityCreated(this)} before returning. */
  private static byte[] entityCreated(byte[] bytes) {
    ClassFile cf = ClassFile.of();
    ClassModel model = cf.parse(bytes);
    ClassTransform t =
        ClassTransform.transformingMethods(
            (MethodModel m) -> m.methodName().equalsString(ConstantDescs.INIT_NAME),
            MethodTransform.transformingCode(
                (b, e) -> {
                  if (e instanceof ReturnInstruction) {
                    b.aload(0);
                    b.invokestatic(
                        HOOKS,
                        "entityCreated",
                        MethodTypeDesc.of(ConstantDescs.CD_void, ConstantDescs.CD_Object));
                  }
                  b.with(e);
                }));
    return cf.transformClass(model, t);
  }
}
