// Copyright Hannes Thöne. Licensed under the Apache License, Version 2.0.
package dev.vanillite.remapper;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Enumeration;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;
import java.util.zip.ZipOutputStream;
import org.objectweb.asm.ClassReader;
import org.objectweb.asm.ClassWriter;
import org.objectweb.asm.Handle;
import org.objectweb.asm.Type;
import org.objectweb.asm.commons.ClassRemapper;
import org.objectweb.asm.commons.Remapper;

/**
 * Applies Mojang's official mappings (ProGuard format) to an obfuscated server jar, so the
 * extractor (written against the unobfuscated names of 26.1 and later) can run against older
 * versions for the multi-version data.
 *
 * <p>Members are resolved up the class hierarchy of the jar; lambda and method reference names
 * follow the functional interface's method. Resources are copied unchanged.
 *
 * <p>Usage: {@code java -cp <asm jars>:<classes> dev.vanillite.remapper.Remap <in.jar> <mappings.txt>
 * <out.jar>}
 */
public final class Remap {
  /** Obfuscated internal class name → named internal class name. */
  private final Map<String, String> classes = new HashMap<>();
  /** Named internal class name → obfuscated internal name. */
  private final Map<String, String> reverse = new HashMap<>();
  /** Obfuscated owner → obfuscated "name descriptor" → named member name. */
  private final Map<String, Map<String, String>> methods = new HashMap<>();
  /** Obfuscated owner → obfuscated field name → named field name. */
  private final Map<String, Map<String, String>> fields = new HashMap<>();
  /** Obfuscated owner → its super class and interfaces (jar classes only). */
  private final Map<String, List<String>> parents = new HashMap<>();

  private Remap() {}

  public static void main(String[] args) throws IOException {
    if (args.length != 3) {
      System.err.println("usage: Remap <in.jar> <mappings.txt> <out.jar>");
      System.exit(2);
    }
    Remap r = new Remap();
    r.load(Path.of(args[1]));
    r.run(Path.of(args[0]), Path.of(args[2]));
  }

  private record Pending(String owner, String ret, String name, String params, String obf) {}

  private void load(Path mappings) throws IOException {
    List<Pending> pending = new ArrayList<>();
    String owner = null;
    try (BufferedReader in = Files.newBufferedReader(mappings, StandardCharsets.UTF_8)) {
      for (String line; (line = in.readLine()) != null; ) {
        if (line.startsWith("#") || line.isBlank()) {
          continue;
        }
        if (!line.startsWith(" ")) {
          // `net.minecraft.Util -> ab:`
          int arrow = line.indexOf(" -> ");
          String named = line.substring(0, arrow).replace('.', '/');
          owner = line.substring(arrow + 4, line.length() - 1).replace('.', '/');
          classes.put(owner, named);
          reverse.put(named, owner);
          continue;
        }
        String s = line.trim();
        int arrow = s.indexOf(" -> ");
        String obf = s.substring(arrow + 4);
        s = s.substring(0, arrow);
        int paren = s.indexOf('(');
        if (paren < 0) {
          // `int field -> a`
          String name = s.substring(s.indexOf(' ') + 1);
          fields.computeIfAbsent(owner, k -> new HashMap<>()).put(obf, name);
          continue;
        }
        // `12:15:void method(int,java.lang.String):40:43 -> b`
        while (Character.isDigit(s.charAt(0))) {
          s = s.substring(s.indexOf(':') + 1);
        }
        int close = s.indexOf(')');
        String params = s.substring(s.indexOf('(') + 1, close);
        String head = s.substring(0, s.indexOf('('));
        int space = head.indexOf(' ');
        pending.add(
            new Pending(owner, head.substring(0, space), head.substring(space + 1), params, obf));
      }
    }
    // Descriptors need every class name, so methods are resolved after the whole file.
    for (Pending p : pending) {
      StringBuilder desc = new StringBuilder("(");
      if (!p.params.isEmpty()) {
        for (String t : p.params.split(",")) {
          desc.append(obfDesc(t));
        }
      }
      desc.append(')').append(obfDesc(p.ret));
      methods.computeIfAbsent(p.owner, k -> new HashMap<>()).put(p.obf + desc, p.name);
    }
  }

  /** The obfuscated descriptor of a Java type name from the mappings (`java.lang.String[]`). */
  private String obfDesc(String type) {
    int dims = 0;
    while (type.endsWith("[]")) {
      dims++;
      type = type.substring(0, type.length() - 2);
    }
    String d =
        switch (type) {
          case "void" -> "V";
          case "boolean" -> "Z";
          case "byte" -> "B";
          case "char" -> "C";
          case "short" -> "S";
          case "int" -> "I";
          case "long" -> "J";
          case "float" -> "F";
          case "double" -> "D";
          default -> {
            String internal = type.replace('.', '/');
            yield "L" + reverse.getOrDefault(internal, internal) + ";";
          }
        };
    return "[".repeat(dims) + d;
  }

  private void run(Path in, Path out) throws IOException {
    try (ZipFile zip = new ZipFile(in.toFile())) {
      Map<String, byte[]> classBytes = new HashMap<>();
      Enumeration<? extends ZipEntry> entries = zip.entries();
      while (entries.hasMoreElements()) {
        ZipEntry e = entries.nextElement();
        if (e.getName().endsWith(".class")) {
          byte[] b = read(zip, e);
          String name = e.getName().substring(0, e.getName().length() - 6);
          classBytes.put(name, b);
          ClassReader cr = new ClassReader(b);
          List<String> ps = new ArrayList<>();
          if (cr.getSuperName() != null) {
            ps.add(cr.getSuperName());
          }
          ps.addAll(List.of(cr.getInterfaces()));
          parents.put(name, ps);
        }
      }
      Remapper remapper = new Mapper();
      try (ZipOutputStream zo = new ZipOutputStream(Files.newOutputStream(out))) {
        entries = zip.entries();
        while (entries.hasMoreElements()) {
          ZipEntry e = entries.nextElement();
          if (e.isDirectory() || e.getName().startsWith("META-INF/") && e.getName().endsWith(".SF")) {
            continue;
          }
          if (e.getName().endsWith(".class")) {
            String name = e.getName().substring(0, e.getName().length() - 6);
            ClassReader cr = new ClassReader(classBytes.get(name));
            ClassWriter cw = new ClassWriter(0);
            cr.accept(new ClassRemapper(cw, remapper), 0);
            zo.putNextEntry(new ZipEntry(remapper.map(name) + ".class"));
            zo.write(cw.toByteArray());
          } else {
            zo.putNextEntry(new ZipEntry(e.getName()));
            zo.write(read(zip, e));
          }
          zo.closeEntry();
        }
      }
    }
  }

  private static byte[] read(ZipFile zip, ZipEntry e) throws IOException {
    try (InputStream s = zip.getInputStream(e)) {
      return s.readAllBytes();
    }
  }

  private final class Mapper extends Remapper {
    @Override
    public String map(String internalName) {
      String named = classes.get(internalName);
      if (named != null) {
        return named;
      }
      // Nested classes the mappings do not list keep their suffix.
      int dollar = internalName.lastIndexOf('$');
      if (dollar > 0) {
        String outer = map(internalName.substring(0, dollar));
        if (outer != null) {
          return outer + internalName.substring(dollar);
        }
      }
      return internalName;
    }

    @Override
    public String mapMethodName(String owner, String name, String descriptor) {
      if (name.startsWith("<")) {
        return name;
      }
      String found = find(owner, name + descriptor, methods, new java.util.HashSet<>());
      return found != null ? found : name;
    }

    @Override
    public String mapFieldName(String owner, String name, String descriptor) {
      String found = find(owner, name, fields, new java.util.HashSet<>());
      return found != null ? found : name;
    }

    @Override
    public String mapRecordComponentName(String owner, String name, String descriptor) {
      return mapFieldName(owner, name, descriptor);
    }

    @Override
    public String mapInvokeDynamicMethodName(
        String name, String descriptor, Handle bsm, Object... args) {
      // LambdaMetafactory: the name is the functional interface's method, args[0] its type.
      if (bsm.getOwner().equals("java/lang/invoke/LambdaMetafactory")
          && args.length > 0
          && args[0] instanceof Type sam) {
        String iface = Type.getReturnType(descriptor).getInternalName();
        return mapMethodName(iface, name, sam.getDescriptor());
      }
      return name;
    }

    private String find(
        String owner,
        String key,
        Map<String, Map<String, String>> table,
        java.util.Set<String> seen) {
      if (!seen.add(owner)) {
        return null;
      }
      Map<String, String> own = table.get(owner);
      if (own != null && own.containsKey(key)) {
        return own.get(key);
      }
      for (String p : parents.getOrDefault(owner, List.of())) {
        String found = find(p, key, table, seen);
        if (found != null) {
          return found;
        }
      }
      return null;
    }
  }
}
